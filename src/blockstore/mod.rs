mod records;

pub use records::{BlockRecord, FileRecord, RecordType};

use crate::{
    db::{Batch, Database, DiskDatabase, Key},
    util::EmptyResult,
};
use bitcoin::{
    consensus::{Decodable, Encodable},
    hashes::Hash,
    BlockHash, Network,
};
use failure::Error;
use log::{debug, info};
use regex::Regex;
use std::{ffi::OsStr, io::Cursor, io::SeekFrom, path::PathBuf};
use tokio::{
    fs::{self, File, OpenOptions},
    prelude::*,
};

pub struct BlockStore {
    db: DiskDatabase,
    options: BlockStoreOptions,
}

impl BlockStore {
    pub fn new(options: BlockStoreOptions) -> Self {
        Self {
            db: DiskDatabase::new(options.index_location.clone()),
            options,
        }
    }

    pub async fn check(&self, record_type: RecordType) -> Result<(bool, Vec<u32>), Error> {
        fn parse_file_number(file_name: &OsStr, regexp: &Regex) -> Result<Option<u32>, Error> {
            let file_name = match file_name.to_str() {
                Some(file_name) => file_name,
                None => return Ok(None),
            };
            if let Some(captures) = regexp.captures(file_name) {
                let file_no = match captures.get(1) {
                    Some(file_no) => file_no,
                    None => return Ok(None),
                };
                Ok(Some(file_no.as_str().parse::<u32>()?))
            } else {
                Ok(None)
            }
        }

        let regexp = regex::Regex::new(&format!("^{}(\\d{{5}})\\.dat$", record_type.prefix()))
            .expect("record check regex fail");

        let mut entries = tokio::fs::read_dir("./").await.unwrap();
        let mut filenos = vec![];

        while let Some(entry) = entries.next_entry().await.unwrap() {
            let file_name = entry.file_name();

            if let Ok(Some(file_no)) = parse_file_number(&file_name, &regexp) {
                filenos.push(file_no);
            }
        }

        let mut missing = false;

        for fileno in &filenos {
            if let None = self
                .db
                .get::<FileRecord>(Key::BlockStoreFile(record_type, *fileno))?
            {
                missing = true;
                break;
            }
        }

        Ok((missing, filenos))
    }

    async fn _index(&mut self, record_type: RecordType) -> EmptyResult {
        use std::io::Seek as SSeek;

        let (missing, filenos) = self.check(record_type).await?;

        if !missing {
            return Ok(());
        }

        let mut batch = Batch::default();

        info!("Indexing block type {:?}...", record_type);

        for fileno in filenos {
            let filepath = self.filepath(record_type, fileno);
            let data: Vec<u8> = tokio::fs::read(&filepath).await?;
            let mut reader: Cursor<&[u8]> = Cursor::new(&data[..]);

            let mut blocks = 0;

            while reader.position() < ((data.len() - 4) as u64) {
                let magic = u32::consensus_decode(&mut reader).expect("Bad magic");
                if magic != self.options.network.magic() {
                    SSeek::seek(&mut reader, SeekFrom::Current(-3)).expect("couldn't seek back 3");
                    continue;
                }

                let length = u32::consensus_decode(&mut reader).expect("bad len");

                let (position, hash) = match record_type {
                    RecordType::Block => {
                        let position = reader.position();

                        if position + 80 > data.len() as u64 {
                            debug!("File is corrupted");
                            continue;
                        }

                        let mut hash_bytes = vec![0; 80];
                        reader.read_exact(&mut hash_bytes).await.expect("read err");
                        let hash = BlockHash::hash(&hash_bytes);
                        (position, hash)
                    }
                };

                let block_record = BlockRecord::new(fileno, position as u32, length);
                blocks += 1;
                batch.insert(Key::BlockStoreBlockRecord(record_type, hash), &block_record)?;
            }

            let file_record = FileRecord::new(
                blocks,
                reader.position() as u32,
                self.options.max_file_length as u32,
            );

            batch.insert(Key::BlockStoreFile(record_type, fileno), &file_record)?;

            self.db
                .write_batch(std::mem::replace(&mut batch, Batch::default()))
                .unwrap();

            info!(
                "Indexed {} blocks (file={}).",
                blocks,
                filepath.to_str().unwrap()
            );
        }

        Ok(())
    }

    async fn index(&mut self) -> EmptyResult {
        self._index(RecordType::Block).await
    }

    async fn ensure(&self) -> EmptyResult {
        fs::create_dir_all(&self.options.index_location).await?;
        Ok(())
    }

    pub async fn open(&mut self) -> EmptyResult {
        info!("Opening FileBlockStore...");
        self.ensure().await?;
        // self.db.verify();
        self.index().await
    }

    pub fn filepath(&self, record_type: RecordType, file_no: u32) -> PathBuf {
        let mut filename = self.options.location.clone();
        filename.push(format!("{}{:0>5}", record_type.prefix(), file_no)); // pad to 5 chars
        filename.set_extension("dat");
        filename
    }

    async fn allocate(
        &self,
        record_type: RecordType,
        length: usize,
    ) -> Result<(u32, FileRecord, PathBuf), Error> {
        if length > self.options.max_file_length {
            panic!("Block length above max file length.")
        }

        let mut fileno: u32 = self
            .db
            .get(Key::BlockStoreLastFile(record_type))?
            .unwrap_or(0);

        let mut filepath = self.filepath(record_type, fileno);

        let mut touch = false;

        let mut filerecord = match self.db.get(Key::BlockStoreFile(record_type, fileno))? {
            Some(file_record) => file_record,
            None => {
                touch = true;
                FileRecord::empty(self.options.max_file_length as u32)
            }
        };

        if filerecord.used + length as u32 > filerecord.length {
            fileno += 1;
            filepath = self.filepath(record_type, fileno);
            touch = true;
            filerecord = FileRecord::empty(self.options.max_file_length as u32);
        }

        if touch {
            File::create(&filepath)
                .await
                .expect("Could't create new file");
        }

        Ok((fileno, filerecord, filepath))
    }

    pub async fn write(&mut self, hash: BlockHash, data: &[u8]) -> EmptyResult {
        self._write(RecordType::Block, hash, data).await
    }

    pub async fn _write(
        &mut self,
        record_type: RecordType,
        hash: BlockHash,
        data: &[u8],
    ) -> EmptyResult {
        if self.db.has(Key::BlockStoreBlockRecord(record_type, hash))? {
            return Ok(());
        }
        let magic_length = 8;

        let block_length = data.len();
        let length = block_length + magic_length;

        let mut magic = Cursor::new(Vec::with_capacity(magic_length));
        self.options
            .network
            .magic()
            .consensus_encode(&mut magic)
            .unwrap();
        (block_length as u32).consensus_encode(&mut magic).unwrap();

        let magic = magic.into_inner();

        let (fileno, mut filerecord, filepath) = self.allocate(record_type, length).await?;

        let magic_postition = filerecord.used as usize;
        let block_position = magic_postition + magic_length;

        let mut file = OpenOptions::new()
            .write(true)
            .read(true)
            .open(filepath)
            .await
            .unwrap();

        file.seek(SeekFrom::Start(magic_postition as u64))
            .await
            .unwrap();

        file.write_all(&magic).await.unwrap();
        file.write_all(data).await.unwrap();

        filerecord.blocks += 1;
        filerecord.used += length as u32;

        let block_record = BlockRecord::new(fileno, block_position as u32, block_length as u32);

        self.db
            .insert(Key::BlockStoreBlockRecord(record_type, hash), &block_record)?;
        self.db
            .insert(Key::BlockStoreFile(record_type, fileno), &filerecord)?;

        self.db
            .insert(Key::BlockStoreLastFile(record_type), &fileno)?;

        Ok(())
    }

    pub async fn read(
        &self,
        hash: BlockHash,
        offset: u32,
        length: Option<u32>,
    ) -> Result<Option<Vec<u8>>, Error> {
        self._read(RecordType::Block, hash, offset, length).await
    }

    async fn _read(
        &self,
        record_type: RecordType,
        hash: BlockHash,
        offset: u32,
        length: Option<u32>,
    ) -> Result<Option<Vec<u8>>, Error> {
        let block_record: BlockRecord =
            match self.db.get(Key::BlockStoreBlockRecord(record_type, hash))? {
                Some(record) => record,
                None => return Ok(None),
            };
        let filepath = self.filepath(record_type, block_record.file);
        let position = if offset > 0 {
            block_record.position + offset
        } else {
            block_record.position
        };
        let length = match length {
            None if offset > 0 => block_record.length - offset,
            None => block_record.length,
            Some(length) => length,
        };
        if offset + length > block_record.length {
            panic!("Out of bounds read");
        }

        let mut data = vec![0; length as usize];
        let mut file = File::open(filepath).await.unwrap();
        file.seek(SeekFrom::Start(position as u64)).await.unwrap();
        let bytes = file.read_exact(&mut data).await.unwrap();

        if bytes != length as usize {
            panic!("Wrong number of bytes read {} vs {}", bytes, length);
        }

        Ok(Some(data))
    }

    pub async fn prune(&mut self, hash: BlockHash) -> Result<bool, Error> {
        self._prune(RecordType::Block, hash).await
    }

    // TODO Batch
    async fn _prune(&mut self, record_type: RecordType, hash: BlockHash) -> Result<bool, Error> {
        let block_record: BlockRecord =
            match self.db.get(Key::BlockStoreBlockRecord(record_type, hash))? {
                Some(record) => record,
                None => return Ok(false),
            };

        let mut file_record: FileRecord = match self
            .db
            .get(Key::BlockStoreFile(record_type, block_record.file))?
        {
            Some(file_record) => file_record,
            None => return Ok(false),
        };
        file_record.blocks -= 1;

        if file_record.blocks == 0 {
            self.db
                .remove(Key::BlockStoreFile(record_type, block_record.file))?;
        } else {
            self.db.insert(
                Key::BlockStoreFile(record_type, block_record.file),
                &file_record,
            )?;
        }

        self.db
            .remove(Key::BlockStoreBlockRecord(record_type, hash))?;

        if file_record.blocks == 0 {
            tokio::fs::remove_file(self.filepath(record_type, block_record.file))
                .await
                .unwrap();
        }

        Ok(true)
    }

    pub async fn has(&self, hash: BlockHash) -> Result<bool, Error> {
        self.db
            .has(Key::BlockStoreBlockRecord(RecordType::Block, hash))
    }
}

pub struct BlockStoreOptions {
    network: Network,
    location: PathBuf,
    max_file_length: usize,
    index_location: PathBuf,
}

impl BlockStoreOptions {
    pub fn new(network: Network, location: String) -> Self {
        let mut location = PathBuf::from(location);
        assert!(location.is_absolute());
        BlockStoreOptions {
            network,
            location: location.clone(),
            max_file_length: 128 * 1024 * 1024,
            index_location: {
                location.push("./index");
                location
            },
        }
    }
}
