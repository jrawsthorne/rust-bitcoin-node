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
    Block, BlockHash, Network, Transaction,
};
use db::COL_BLOCK_RECORD;

pub trait BlockStorage {
    fn write_block(&mut self, hash: BlockHash, block: &Block);
    fn write_undo(&mut self, hash: BlockHash, undo: &UndoCoins);
    fn read_undo(&self, hash: BlockHash) -> Result<Option<Vec<u8>>, DBError>;
}

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
        let mut block_store = Self {
            db: DiskDatabase::new(options.index_location.clone(), db::columns()),
            options,
        };
        block_store.open().unwrap();
        block_store
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

        let mut filenos = vec![];

        for entry in fs::read_dir(&self.options.location)? {
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

    fn index(&mut self, record_type: RecordType) -> Result<(), DBError> {
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
                    reader
                        .seek(SeekFrom::Current(-3))
                        .expect("couldn't seek back 3");
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

    fn index_all_types(&mut self) -> Result<(), DBError> {
        self.index(RecordType::Block)?;
        self.index(RecordType::Undo)
    }

    fn ensure(&self) -> Result<(), DBError> {
        fs::create_dir_all(&self.options.index_location)
            .map_err(|_| DBError::Other("create dir error"))?;
        Ok(())
    }

    fn open(&mut self) -> Result<(), DBError> {
        info!("Opening FileBlockStore...");
        self.ensure()?;
        // self.db.verify();
        self.index_all_types()
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
        if length > self.options.max_file_length as usize {
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

    pub fn write_block(&mut self, hash: BlockHash, block: &Block) -> Result<(), DBError> {
        self.write(RecordType::Block, hash, &serialize(&block))
    }

    pub fn write_undo(&mut self, hash: BlockHash, undo: &UndoCoins) -> Result<(), DBError> {
        self.write(RecordType::Undo, hash, &serialize(undo))
    }

    fn write(
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

    pub fn read_transaction(
        &self,
        block_hash: BlockHash,
        offset: u32,
        length: u32,
    ) -> Result<Option<Transaction>, DBError> {
        let bytes = self.read(RecordType::Block, block_hash, offset, Some(length))?;
        if let Some(bytes) = bytes {
            Ok(Some(Transaction::consensus_decode(&bytes[..])?))
        } else {
            Ok(None)
        }
    }

    pub fn read_block(&self, hash: BlockHash) -> Result<Option<Block>, DBError> {
        let bytes = self.read(RecordType::Block, hash, 0, None)?;
        if let Some(bytes) = bytes {
            Ok(Some(Block::consensus_decode(&bytes[..])?))
        } else {
            Ok(None)
        }
    }

    pub fn read_raw_block(&self, hash: BlockHash) -> Result<Option<Vec<u8>>, DBError> {
        self.read(RecordType::Block, hash, 0, None)
    }

    pub fn read_undo(&self, hash: BlockHash) -> Result<Option<UndoCoins>, DBError> {
        let bytes = self.read(RecordType::Undo, hash, 0, None)?;
        if let Some(bytes) = bytes {
            Ok(Some(UndoCoins::consensus_decode(&bytes[..])?))
        } else {
            Ok(None)
        }
    }

    fn read(
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
        let position = block_record.position + offset;
        let length = match length {
            None if offset > 0 => block_record
                .length
                .checked_sub(offset)
                .expect("Out of bounds read: offset > block length"),
            None => block_record.length,
            Some(length) => length,
        };
        assert!(
            offset + length <= block_record.length,
            "Out of bounds read: end > block length"
        );
        let mut data = vec![0; length as usize];
        let mut file = File::open(filepath)?;
        file.seek(SeekFrom::Start(position as u64))?;
        file.read_exact(&mut data)?;

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
    max_file_length: u32,
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
            index_location: location.join("./index"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use bitcoin::{blockdata::constants::genesis_block, BlockHeader, Script, VarInt};
    use tempfile::TempDir;

    fn init_logger() {
        let _ = env_logger::builder()
            .format_timestamp_millis()
            .is_test(true)
            .try_init();
    }

    #[test]
    fn test_reindex() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        info!("Create blockstore");
        {
            let mut store =
                BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
            let block = genesis_block(Network::Bitcoin);
            store.write_block(block.block_hash(), &block).unwrap();
        }
        info!("Delete database index");
        fs::remove_dir_all(tmpdir.path().join("index")).unwrap();
        info!("Reopen block store");
        {
            let store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
            let expected_block = genesis_block(Network::Bitcoin);
            info!("Fetch expected block from store");
            assert_eq!(
                store.read_block(expected_block.block_hash()).unwrap(),
                Some(expected_block)
            )
        }
    }

    #[test]
    fn test_prune_block() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        store.write_block(block.block_hash(), &block).unwrap();
        assert!(store.has(block.block_hash()).unwrap());
        assert!(tmpdir.path().join("blk00000.dat").exists());
        store.prune(block.block_hash()).unwrap();
        assert!(!store.has(block.block_hash()).unwrap());
        assert!(!tmpdir.path().join("blk00000.dat").exists());
    }

    #[test]
    fn test_prune_multiple_blocks() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let first_block = genesis_block(Network::Bitcoin);
        let second_block = {
            let mut block = first_block.clone();
            block.header.nonce += 1;
            block
        };
        store
            .write_block(first_block.block_hash(), &first_block)
            .unwrap();
        store
            .write_block(second_block.block_hash(), &second_block)
            .unwrap();

        assert!(store.has(first_block.block_hash()).unwrap());
        assert!(store.has(second_block.block_hash()).unwrap());
        assert!(tmpdir.path().join("blk00000.dat").exists());

        assert!(store.prune(first_block.block_hash()).unwrap());

        assert!(!store.has(first_block.block_hash()).unwrap());
        assert!(store.has(second_block.block_hash()).unwrap());
        assert!(tmpdir.path().join("blk00000.dat").exists());

        assert!(store.prune(second_block.block_hash()).unwrap());

        assert!(!store.has(second_block.block_hash()).unwrap());
        assert!(!tmpdir.path().join("blk00000.dat").exists());
    }

    #[test]
    fn test_prune_non_existent_block() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let hash = genesis_block(Network::Bitcoin).block_hash();
        assert!(!store.prune(hash).unwrap());
    }

    #[test]
    fn test_max_file_length() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut options = BlockStoreOptions::new(Network::Bitcoin, tmpdir.path());

        let mut block = genesis_block(Network::Bitcoin);
        // on disk block size (account for 8 bytes of meta)
        let block_length = block.get_size() as u32;
        let block_length_with_meta = block_length + 8;

        let max_file_length = 10 * block_length_with_meta;
        options.max_file_length = max_file_length;
        let mut store = BlockStore::new(options);

        // write 10 blocks to first file
        for i in 0..10 {
            block.header.nonce = i;
            store.write_block(block.block_hash(), &block).unwrap();
            let file_record: FileRecord = store
                .db
                .get(Key::File(RecordType::Block, 0))
                .unwrap()
                .unwrap();
            let block_record: BlockRecord = store
                .db
                .get(Key::BlockRecord(RecordType::Block, block.block_hash()))
                .unwrap()
                .unwrap();
            assert_eq!(file_record.blocks, i + 1);
            assert_eq!(file_record.used, (i + 1) * block_length_with_meta);
            assert_eq!(file_record.length, max_file_length);

            assert_eq!(block_record.file, 0);
            assert_eq!(block_record.position, (i * block_length_with_meta) + 8);
            assert_eq!(block_record.length, block_length);
        }

        // write one more block to second file
        block.header.nonce = 10;
        store.write_block(block.block_hash(), &block).unwrap();
        let file_record: FileRecord = store
            .db
            .get(Key::File(RecordType::Block, 1))
            .unwrap()
            .unwrap();
        let block_record: BlockRecord = store
            .db
            .get(Key::BlockRecord(RecordType::Block, block.block_hash()))
            .unwrap()
            .unwrap();
        assert_eq!(file_record.blocks, 1);
        assert_eq!(file_record.used, block_length_with_meta);
        assert_eq!(file_record.length, max_file_length);

        assert_eq!(block_record.file, 1);
        assert_eq!(block_record.position, 8);
        assert_eq!(block_record.length, block_length);
    }

    #[test]
    #[should_panic(expected = "Out of bounds read: offset > block length")]
    fn test_out_of_bounds_offset() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        let block_size = block.get_size() as u32;
        store.write_block(block.block_hash(), &block).unwrap();
        // read with offset past end of block
        store
            .read(RecordType::Block, block.block_hash(), block_size + 1, None)
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "Out of bounds read: end > block length")]
    fn test_out_of_bounds_length() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        // on disk block size (account for 8 bytes of meta)
        let block_size = block.get_size() as u32 + 8;
        store.write_block(block.block_hash(), &block).unwrap();
        // read with length past end of block
        store
            .read(
                RecordType::Block,
                block.block_hash(),
                0,
                Some(block_size + 1),
            )
            .unwrap();
    }

    #[test]
    fn test_read_offset() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        store.write_block(block.block_hash(), &block).unwrap();
        // the coinbase transaction is after the header (80 bytes) and varint for num of transactions (1 byte for 1 tx)
        let coinbase_bytes = store
            .read(RecordType::Block, block.block_hash(), 81, None)
            .unwrap()
            .unwrap();
        assert_eq!(block.get_size() - 81, coinbase_bytes.len());
        let coinbase = Transaction::consensus_decode(&coinbase_bytes[..]).unwrap();
        assert_eq!(coinbase, block.txdata[0]);
    }

    #[test]
    fn test_read_length() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        store.write_block(block.block_hash(), &block).unwrap();
        // the first 80 bytes of a block are the header
        let header_bytes = store
            .read(RecordType::Block, block.block_hash(), 0, Some(80))
            .unwrap()
            .unwrap();
        assert_eq!(80, header_bytes.len());
        let header = BlockHeader::consensus_decode(&header_bytes[..]).unwrap();
        assert_eq!(header, block.header);
    }

    #[test]
    fn test_read_offset_and_length() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        store.write_block(block.block_hash(), &block).unwrap();
        // script starts at block header + num txs varint len + version + num inputs varint len + outpoint
        let script_start = 80 + 1 + 4 + 1 + 36;
        // len of script bytes + 1 byte for script length varint
        let script_length = {
            let raw = block.txdata[0].input[0].script_sig.len();
            raw + VarInt(raw as u64).len()
        };
        let script_bytes = store
            .read(
                RecordType::Block,
                block.block_hash(),
                script_start,
                Some(script_length as u32),
            )
            .unwrap()
            .unwrap();
        assert_eq!(script_length, script_bytes.len());
        let script = Script::consensus_decode(&script_bytes[..]).unwrap();
        assert_eq!(script, block.txdata[0].input[0].script_sig);
    }

    #[test]
    fn test_read_transaction() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        store.write_block(block.block_hash(), &block).unwrap();
        let transaction = store
            .read_transaction(block.block_hash(), 81, block.txdata[0].get_size() as u32)
            .unwrap()
            .unwrap();
        assert_eq!(transaction, block.txdata[0]);
    }

    #[test]
    fn test_has() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        store.write_block(block.block_hash(), &block).unwrap();
        assert!(store.has(block.block_hash()).unwrap());

        let mut block = block;
        block.header.nonce += 1;
        assert!(!store.has(block.block_hash()).unwrap());
    }

    #[test]
    fn test_read_raw_block() {
        init_logger();
        let tmpdir = TempDir::new().unwrap();
        let mut store = BlockStore::new(BlockStoreOptions::new(Network::Bitcoin, tmpdir.path()));
        let block = genesis_block(Network::Bitcoin);
        store.write_block(block.block_hash(), &block).unwrap();
        let block_bytes = store.read_raw_block(block.block_hash()).unwrap().unwrap();
        assert_eq!(block_bytes, serialize(&block));
    }
}
