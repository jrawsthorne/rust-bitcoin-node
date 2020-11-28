mod db;
mod records;

pub use db::Key;
pub use records::{BlockRecord, FileRecord, RecordType};

use crate::{
    coins::UndoCoins,
    db::{Batch, Database, DiskDatabase, IterMode},
    error::DBError,
};
use bitcoin::{
    consensus::serialize,
    consensus::{deserialize, Decodable, Encodable},
    hashes::Hash,
    BlockHash, Network,
};
use db::COL_BLOCK_RECORD;

use log::{debug, info};
use regex::Regex;
use std::fs::{self, File, OpenOptions};
use std::{
    collections::HashSet, ffi::OsStr, io::prelude::*, io::Cursor, io::SeekFrom, path::PathBuf,
};

pub struct BlockStore {
    db: DiskDatabase,
    options: BlockStoreOptions,
}

impl BlockStore {
    pub fn new(options: BlockStoreOptions) -> Self {
        Self {
            db: DiskDatabase::new(options.index_location.clone(), db::columns()),
            options,
        }
    }

    pub fn downloaded_set(&self) -> HashSet<BlockHash> {
        let mut downloaded = HashSet::new();
        for (hash, _) in self
            .db
            .iter_cf::<Key, BlockRecord>(COL_BLOCK_RECORD, IterMode::Start)
            .unwrap()
        {
            let hash = &hash[4..];
            let hash = deserialize(&hash).unwrap();
            downloaded.insert(hash);
        }
        downloaded
    }

    pub fn check(&self, record_type: RecordType) -> Result<(bool, Vec<u32>), DBError> {
        fn parse_file_number(file_name: &OsStr, regexp: &Regex) -> Result<Option<u32>, DBError> {
            let file_name = match file_name.to_str() {
                Some(file_name) => file_name,
                None => return Ok(None),
            };
            if let Some(captures) = regexp.captures(file_name) {
                let file_no = match captures.get(1) {
                    Some(file_no) => file_no,
                    None => return Ok(None),
                };
                Ok(Some(
                    file_no
                        .as_str()
                        .parse::<u32>()
                        .map_err(|_| DBError::Other("file parse error"))?,
                ))
            } else {
                Ok(None)
            }
        }

        let regexp = regex::Regex::new(&format!("^{}(\\d{{5}})\\.dat$", record_type.prefix()))
            .expect("record check regex fail");

        let mut entries = fs::read_dir("./")?;
        let mut filenos = vec![];

        while let Some(entry) = entries.next() {
            let entry = entry.map_err(|_| DBError::Other("read dir error"))?;
            let file_name = entry.file_name();

            if let Ok(Some(file_no)) = parse_file_number(&file_name, &regexp) {
                filenos.push(file_no);
            }
        }

        let mut missing = false;

        for fileno in &filenos {
            if let None = self
                .db
                .get::<_, FileRecord>(Key::File(record_type, *fileno))?
            {
                missing = true;
                break;
            }
        }

        Ok((missing, filenos))
    }

    fn _index(&mut self, record_type: RecordType) -> Result<(), DBError> {
        use std::io::Seek as SSeek;

        let (missing, filenos) = self.check(record_type)?;

        if !missing {
            return Ok(());
        }

        let mut batch = Batch::new();

        info!("Indexing block type {:?}...", record_type);

        for fileno in filenos {
            let filepath = self.filepath(record_type, fileno);
            let data: Vec<u8> = fs::read(&filepath).map_err(|_| DBError::Other("fs read error"))?;
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

                        let mut hash_bytes = [0; 80];
                        reader.read_exact(&mut hash_bytes).expect("read err");
                        let hash = BlockHash::hash(&hash_bytes);
                        (position, hash)
                    }
                    RecordType::Undo => {
                        let mut hash_bytes = [0; 32];
                        reader.read_exact(&mut hash_bytes)?;
                        let position = reader.position();
                        let hash = BlockHash::from_slice(&hash_bytes).expect("malformed hash");
                        (position, hash)
                    }
                };

                let block_record = BlockRecord::new(fileno, position as u32, length);
                blocks += 1;
                batch.insert(Key::BlockRecord(record_type, hash), &block_record);
            }

            let file_record = FileRecord::new(
                blocks,
                reader.position() as u32,
                self.options.max_file_length as u32,
            );

            batch.insert(Key::File(record_type, fileno), &file_record);

            self.db
                .write_batch(std::mem::replace(&mut batch, Batch::new()))?;

            info!(
                "Indexed {} blocks (file={}).",
                blocks,
                filepath.to_str().unwrap()
            );
        }

        Ok(())
    }

    fn index(&mut self) -> Result<(), DBError> {
        self._index(RecordType::Block)?;
        self._index(RecordType::Undo)
    }

    fn ensure(&self) -> Result<(), DBError> {
        fs::create_dir_all(&self.options.index_location)
            .map_err(|_| DBError::Other("create dir error"))?;
        Ok(())
    }

    pub fn open(&mut self) -> Result<(), DBError> {
        info!("Opening FileBlockStore...");
        self.ensure()?;
        // self.db.verify();
        self.index()
    }

    pub fn filepath(&self, record_type: RecordType, file_no: u32) -> PathBuf {
        let mut filename = self.options.location.clone();
        filename.push(format!("{}{:0>5}", record_type.prefix(), file_no)); // pad to 5 chars
        filename.set_extension("dat");
        filename
    }

    fn allocate(
        &self,
        record_type: RecordType,
        length: usize,
    ) -> Result<(u32, FileRecord, PathBuf), DBError> {
        if length > self.options.max_file_length {
            panic!("Block length above max file length.")
        }

        let mut fileno: u32 = self.db.get(Key::LastFile(record_type))?.unwrap_or(0);

        let mut filepath = self.filepath(record_type, fileno);

        let mut touch = false;

        let mut filerecord = match self.db.get(Key::File(record_type, fileno))? {
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
            File::create(&filepath).expect("Could't create new file");
        }

        Ok((fileno, filerecord, filepath))
    }

    pub fn write(&mut self, hash: BlockHash, data: &[u8]) -> Result<(), DBError> {
        self._write(RecordType::Block, hash, data)
    }

    pub fn write_undo(&mut self, hash: BlockHash, undo: &UndoCoins) -> Result<(), DBError> {
        self._write(RecordType::Undo, hash, &serialize(undo))
    }

    pub fn _write(
        &mut self,
        record_type: RecordType,
        hash: BlockHash,
        data: &[u8],
    ) -> Result<(), DBError> {
        if self.db.has(Key::BlockRecord(record_type, hash))? {
            return Ok(());
        }
        let mut magic_length = 8;

        if let RecordType::Undo = record_type {
            magic_length += 32;
        }

        let block_length = data.len();
        let length = block_length + magic_length;

        let mut magic = Vec::with_capacity(8);

        self.options.network.magic().consensus_encode(&mut magic)?;
        (block_length as u32).consensus_encode(&mut magic)?;

        if let RecordType::Undo = record_type {
            hash.consensus_encode(&mut magic)?;
        }

        let (fileno, mut filerecord, filepath) = self.allocate(record_type, length)?;

        let magic_postition = filerecord.used as usize;
        let block_position = magic_postition + magic_length;

        let mut file = OpenOptions::new().write(true).read(true).open(filepath)?;

        file.seek(SeekFrom::Start(magic_postition as u64))?;

        file.write_all(&magic)?;
        file.write_all(data)?;

        file.flush()?;

        filerecord.blocks += 1;
        filerecord.used += length as u32;

        let block_record = BlockRecord::new(fileno, block_position as u32, block_length as u32);

        let mut batch = Batch::new();

        batch.insert(Key::BlockRecord(record_type, hash), &block_record);
        batch.insert(Key::File(record_type, fileno), &filerecord);

        batch.insert(Key::LastFile(record_type), &fileno);

        self.db.write_batch(batch)?;

        Ok(())
    }

    pub fn read(
        &self,
        hash: BlockHash,
        offset: u32,
        length: Option<u32>,
    ) -> Result<Option<Vec<u8>>, DBError> {
        self._read(RecordType::Block, hash, offset, length)
    }

    pub fn read_undo(&self, hash: BlockHash) -> Result<Option<Vec<u8>>, DBError> {
        self._read(RecordType::Undo, hash, 0, None)
    }

    fn _read(
        &self,
        record_type: RecordType,
        hash: BlockHash,
        offset: u32,
        length: Option<u32>,
    ) -> Result<Option<Vec<u8>>, DBError> {
        let block_record: BlockRecord = match self.db.get(Key::BlockRecord(record_type, hash))? {
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
        let mut file = File::open(filepath)?;
        file.seek(SeekFrom::Start(position as u64))?;
        file.read_exact(&mut data)?;

        // if bytes != length as usize {
        //     panic!("Wrong number of bytes read {} vs {}", bytes, length);
        // }

        Ok(Some(data))
    }

    pub fn prune(&mut self, hash: BlockHash) -> Result<bool, DBError> {
        self._prune(RecordType::Block, hash)
    }

    // TODO Batch
    fn _prune(&mut self, record_type: RecordType, hash: BlockHash) -> Result<bool, DBError> {
        let block_record: BlockRecord = match self.db.get(Key::BlockRecord(record_type, hash))? {
            Some(record) => record,
            None => return Ok(false),
        };

        let mut batch = Batch::new();

        let mut file_record: FileRecord =
            match self.db.get(Key::File(record_type, block_record.file))? {
                Some(file_record) => file_record,
                None => return Ok(false),
            };
        file_record.blocks -= 1;

        if file_record.blocks == 0 {
            batch.remove(Key::File(record_type, block_record.file));
        } else {
            batch.insert(Key::File(record_type, block_record.file), &file_record);
        }

        batch.remove(Key::BlockRecord(record_type, hash));

        if file_record.blocks == 0 {
            fs::remove_file(self.filepath(record_type, block_record.file))?;
        }

        self.db.write_batch(batch)?;

        Ok(true)
    }

    pub fn has(&self, hash: BlockHash) -> Result<bool, DBError> {
        self.db.has(Key::BlockRecord(RecordType::Block, hash))
    }
}

pub struct BlockStoreOptions {
    network: Network,
    location: PathBuf,
    max_file_length: usize,
    index_location: PathBuf,
}

impl BlockStoreOptions {
    pub fn new(network: Network, location: impl Into<PathBuf>) -> Self {
        let location = location.into();
        // assert!(location.is_absolute());
        BlockStoreOptions {
            network,
            location: location.clone(),
            max_file_length: 128 * 1024 * 1024,
            index_location: { location.join("./index") },
        }
    }
}
