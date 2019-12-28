use super::{batch::Operation, Batch, DBValue, Database, Key};
use crate::util::EmptyResult;
use failure::{err_msg, Error};
use rocksdb::{ColumnFamily, Options, WriteBatch, DB};

pub const COL_CHAIN_ENTRY: &'static str = "0";
pub const COL_CHAIN_ENTRY_HEIGHT: &'static str = "1";
pub const COL_CHAIN_ENTRY_HASH: &'static str = "2";
pub const COL_COIN: &'static str = "3";
pub const COL_NEXT_HASH: &'static str = "4";
pub const COL_MISC: &'static str = "5";

pub const KEY_TIP: [u8; 1] = [0];
pub const KEY_CHAIN_STATE: [u8; 1] = [1];

pub const COLUMNS: [&'static str; 6] = [
    COL_CHAIN_ENTRY,
    COL_CHAIN_ENTRY_HEIGHT,
    COL_CHAIN_ENTRY_HASH,
    COL_COIN,
    COL_NEXT_HASH,
    COL_MISC,
];

pub struct DiskDatabase {
    db: DB,
}

use std::path::PathBuf;

impl DiskDatabase {
    pub fn new(path: PathBuf) -> Self {
        let mut db_options = Options::default();
        db_options.create_if_missing(true);
        db_options.create_missing_column_families(true);

        Self {
            db: DB::open_cf(&db_options, path, &COLUMNS).unwrap(),
        }
    }

    fn col(&self, col: &'static str) -> Result<&ColumnFamily, Error> {
        self.db.cf_handle(col).ok_or_else(|| err_msg("bad column"))
    }
}

impl Database for DiskDatabase {
    fn insert<V: DBValue>(&self, key: Key, value: &V) -> EmptyResult {
        let col = self.col(key.col())?;
        self.db.put_cf(col, key.encode()?, value.encode()?)?;
        Ok(())
    }

    fn remove(&self, key: Key) -> EmptyResult {
        let col = self.col(key.col())?;
        self.db.delete_cf(col, key.encode()?)?;
        Ok(())
    }

    fn get<V: DBValue>(&self, key: Key) -> Result<Option<V>, Error> {
        let col = self.col(key.col())?;
        let raw = self.db.get_cf(col, key.encode()?)?;
        Ok(match raw {
            Some(raw) => Some(V::decode(&raw)?),
            None => None,
        })
    }

    fn write_batch(&self, batch: Batch) -> EmptyResult {
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
}
