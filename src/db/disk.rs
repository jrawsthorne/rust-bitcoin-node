use super::{batch::Operation, Batch, DBValue, Database, Key};
use crate::error::DBError;
use rocksdb::{ColumnFamily, DBIterator, Direction, IteratorMode, Options, WriteBatch, DB};
use std::marker::PhantomData;
use std::path::PathBuf;

pub const COL_CHAIN_ENTRY: &str = "0";
pub const COL_CHAIN_ENTRY_HEIGHT: &str = "1";
pub const COL_CHAIN_ENTRY_HASH: &str = "2";
pub const COL_COIN: &str = "3";
pub const COL_NEXT_HASH: &str = "4";
pub const COL_MISC: &str = "5";
pub const COL_CHAIN_WORK: &str = "6";
pub const COL_CHAIN_NEXT_HASHES: &str = "7";
pub const COL_CHAIN_SKIP: &str = "8";
pub const COL_BLOCKSTORE_FILE: &str = "9";
pub const COL_BLOCKSTORE_LAST_FILE: &str = "10";
pub const COL_BLOCKSTORE_BLOCK_RECORD: &str = "11";
pub const COL_VERSION_BIT_STATE: &str = "12";

pub const KEY_TIP: [u8; 1] = [0];
pub const KEY_CHAIN_STATE: [u8; 1] = [1];

pub const COLUMNS: [&str; 13] = [
    COL_CHAIN_ENTRY,
    COL_CHAIN_ENTRY_HEIGHT,
    COL_CHAIN_ENTRY_HASH,
    COL_COIN,
    COL_NEXT_HASH,
    COL_MISC,
    COL_CHAIN_WORK,
    COL_CHAIN_NEXT_HASHES,
    COL_CHAIN_SKIP,
    COL_BLOCKSTORE_FILE,
    COL_BLOCKSTORE_LAST_FILE,
    COL_BLOCKSTORE_BLOCK_RECORD,
    COL_VERSION_BIT_STATE,
];

pub struct DiskDatabase {
    db: DB,
}

pub struct Iter<'a, V: DBValue> {
    iter: DBIterator<'a>,
    v: PhantomData<V>,
}

impl<'a, V: DBValue> Iterator for Iter<'a, V> {
    type Item = (Box<[u8]>, V);
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(next) = self.iter.next() {
            let value = V::decode(&next.1);
            if let Ok(value) = value {
                return Some((next.0, value));
            }
        }
        None
    }
}

pub enum IterMode {
    Start,
    End,
    From(Key, IterDirection),
}

pub enum IterDirection {
    Forward,
    Reverse,
}

impl DiskDatabase {
    pub fn new(path: PathBuf) -> Self {
        let mut db_options = Options::default();
        db_options.create_if_missing(true);
        db_options.create_missing_column_families(true);
        db_options.increase_parallelism(4);
        db_options.set_compression_type(rocksdb::DBCompressionType::Snappy);

        let db = Self {
            db: DB::open_cf(&db_options, path, &COLUMNS).unwrap(),
        };

        db.compact();

        db
    }

    fn compact(&self) {
        for column in &COLUMNS {
            let col = self.col(column).unwrap();
            self.db
                .compact_range_cf::<Vec<u8>, Vec<u8>>(col, None, None);
        }
    }

    fn col(&self, col: &'static str) -> Result<&ColumnFamily, DBError> {
        self.db
            .cf_handle(col)
            .ok_or_else(|| DBError::Other("bad column"))
    }

    pub fn iter_cf<V: DBValue>(
        &self,
        col: &'static str,
        mode: IterMode,
    ) -> Result<Iter<V>, DBError> {
        let col = self.col(col)?;

        let from_key = if let IterMode::From(key, _) = &mode {
            Some(key.encode()?)
        } else {
            None
        };

        let mode = match mode {
            IterMode::End => IteratorMode::End,
            IterMode::Start => IteratorMode::Start,
            IterMode::From(_, direction) => {
                let direction = match direction {
                    IterDirection::Forward => Direction::Forward,
                    IterDirection::Reverse => Direction::Reverse,
                };
                IteratorMode::From(from_key.as_ref().unwrap(), direction)
            }
        };

        let iter = self.db.iterator_cf(col, mode)?;

        Ok(Iter {
            iter,
            v: PhantomData,
        })
    }
}

impl Database for DiskDatabase {
    fn insert<V: DBValue>(&self, key: Key, value: &V) -> Result<(), DBError> {
        let col = self.col(key.col())?;
        self.db.put_cf(col, key.encode()?, value.encode()?)?;
        Ok(())
    }

    fn remove(&self, key: Key) -> Result<(), DBError> {
        let col = self.col(key.col())?;
        self.db.delete_cf(col, key.encode()?)?;
        Ok(())
    }

    fn get<V: DBValue>(&self, key: Key) -> Result<Option<V>, DBError> {
        let col = self.col(key.col())?;
        let raw = self.db.get_pinned_cf(col, key.encode()?)?;
        Ok(match raw {
            Some(raw) => Some(V::decode(&raw)?),
            None => None,
        })
    }

    fn write_batch(&self, batch: Batch) -> Result<(), DBError> {
        let mut write_batch = WriteBatch::default();
        for operation in batch.operations {
            match operation {
                Operation::Insert(key, value) => {
                    let col = self.col(key.col())?;
                    write_batch.put_cf(col, key.encode()?, value)?;
                }
                Operation::Remove(key) => {
                    let col = self.col(key.col())?;
                    write_batch.delete_cf(col, key.encode()?)?;
                }
            }
        }
        self.db.write(write_batch)?;
        Ok(())
    }

    fn has(&self, key: Key) -> Result<bool, DBError> {
        let col = self.col(key.col())?;
        let value = self.db.get_pinned_cf(col, key.encode()?)?;
        Ok(value.is_some())
    }
}
