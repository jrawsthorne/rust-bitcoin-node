use super::{batch::Operation, Batch, DBKey, Database};
use crate::error::DBError;
use bitcoin::consensus::{serialize, Decodable};
use rocksdb::{ColumnFamily, DBIterator, Direction, IteratorMode, Options, WriteBatch, DB};
use std::marker::PhantomData;
use std::path::Path;

pub const KEY_CHAIN_STATE: [u8; 1] = [1];

pub struct DiskDatabase {
    db: DB,
    columns: Vec<&'static str>,
}

pub struct Iter<'a, V: Decodable> {
    iter: DBIterator<'a>,
    v: PhantomData<V>,
}

impl<'a, V: Decodable> Iterator for Iter<'a, V> {
    type Item = (Box<[u8]>, V);
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(next) = self.iter.next() {
            let (key, value) = next.expect("TODO: handle iterator error");
            let value = V::consensus_decode(&value[..]);
            if let Ok(value) = value {
                return Some((key, value));
            }
        }
        None
    }
}

pub enum IterMode<K: DBKey> {
    Start,
    End,
    From(K, IterDirection),
}

pub enum IterDirection {
    Forward,
    Reverse,
}

impl DiskDatabase {
    pub fn new(path: impl AsRef<Path>, columns: Vec<&'static str>) -> Self {
        let mut columns_to_open: Vec<String> = columns.iter().map(|col| col.to_string()).collect();

        let mut db_options = Options::default();
        db_options.create_if_missing(true);
        db_options.create_missing_column_families(true);
        db_options.increase_parallelism(4);

        // Collect any existing columns that are no longer used
        let mut cfs_to_drop = vec![];

        if let Ok(cfs) = DB::list_cf(&db_options, &path) {
            for cf in cfs {
                if cf != "default" && !columns_to_open.contains(&cf) {
                    cfs_to_drop.push(cf);
                }
            }
        } else {
            // no existing db
        }

        // Extend the list of columns to open with old columns
        for cf in &cfs_to_drop {
            columns_to_open.push(cf.clone());
        }

        let mut db = Self {
            db: DB::open_cf(&db_options, path, &columns_to_open).unwrap(),
            columns,
        };

        // Drop old columns
        for cf in &cfs_to_drop {
            db.db.drop_cf(cf).unwrap();
        }

        db.compact();

        db
    }

    pub fn compact(&self) {
        for column in &self.columns {
            let col = self.col(column);
            self.db
                .compact_range_cf(col, Option::<Vec<u8>>::None, Option::<Vec<u8>>::None);
        }
    }

    fn col(&self, col: &str) -> &ColumnFamily {
        self.db.cf_handle(col).expect("column doesn't exist")
    }

    pub fn iter_cf<K: DBKey, V: Decodable>(
        &self,
        col: &'static str,
        mode: IterMode<K>,
    ) -> Result<Iter<V>, DBError> {
        let col = self.col(col);

        let from_key = if let IterMode::From(key, _) = &mode {
            Some(serialize(key))
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

        let iter = self.db.iterator_cf(col, mode);

        Ok(Iter {
            iter,
            v: PhantomData,
        })
    }
}

impl Database for DiskDatabase {
    fn get<K: DBKey, V: Decodable>(&self, key: K) -> Result<Option<V>, DBError> {
        let col = self.col(key.col());
        let raw = self.db.get_pinned_cf(col, serialize(&key))?;
        Ok(match raw {
            Some(raw) => Some(V::consensus_decode(&raw[..])?),
            None => None,
        })
    }

    fn write_batch<K: DBKey>(&self, batch: Batch<K>) -> Result<(), DBError> {
        let mut write_batch = WriteBatch::default();
        let mut key_buf = vec![];

        for operation in batch.operations {
            match operation {
                Operation::Insert(key, value) => {
                    let col = self.col(key.col());
                    key.consensus_encode(&mut key_buf).unwrap();
                    write_batch.put_cf(col, &key_buf, value);
                    key_buf.clear();
                }
                Operation::Remove(key) => {
                    let col = self.col(key.col());
                    key.consensus_encode(&mut key_buf).unwrap();
                    write_batch.delete_cf(col, &key_buf);
                    key_buf.clear();
                }
            }
        }
        self.db.write(write_batch)?;
        Ok(())
    }

    fn has<K: DBKey>(&self, key: K) -> Result<bool, DBError> {
        let col = self.col(key.col());
        let value = self.db.get_pinned_cf(col, serialize(&key))?;
        Ok(value.is_some())
    }
}
