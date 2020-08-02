use super::{batch::Operation, Batch, DBKey, DBValue, Database};
use crate::error::DBError;
use rocksdb::{ColumnFamily, DBIterator, Direction, IteratorMode, Options, WriteBatch, DB};
use std::marker::PhantomData;
use std::path::Path;

pub const KEY_TIP: [u8; 1] = [0];
pub const KEY_CHAIN_STATE: [u8; 1] = [1];

pub struct DiskDatabase<K: DBKey> {
    db: DB,
    columns: Vec<&'static str>,
    _key: PhantomData<K>,
}

pub struct Iter<'a, V: DBValue> {
    iter: DBIterator<'a>,
    v: PhantomData<V>,
}

impl<'a, V: DBValue> Iterator for Iter<'a, V> {
    type Item = (Box<[u8]>, V);
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(next) = self.iter.next() {
            let value = V::decode(&next.1[..]);
            if let Ok(value) = value {
                return Some((next.0, value));
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

impl<K: DBKey> DiskDatabase<K> {
    pub fn new(path: impl AsRef<Path>, columns: Vec<&'static str>) -> Self {
        let mut db_options = Options::default();
        db_options.create_if_missing(true);
        db_options.create_missing_column_families(true);
        db_options.increase_parallelism(4);

        let db = Self {
            db: DB::open_cf(&db_options, path, &columns).unwrap(),
            columns,
            _key: PhantomData::default(),
        };

        db
    }

    pub fn compact(&self) {
        for column in &self.columns {
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
        mode: IterMode<K>,
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

        let iter = self.db.iterator_cf(col, mode);

        Ok(Iter {
            iter,
            v: PhantomData,
        })
    }
}

impl<K: DBKey> Database<K> for DiskDatabase<K> {
    fn insert(&self, key: K, value: &impl DBValue) -> Result<(), DBError> {
        let col = self.col(key.col())?;
        self.db.put_cf(col, key.encode()?, value.encode()?)?;
        Ok(())
    }

    fn remove(&self, key: K) -> Result<(), DBError> {
        let col = self.col(key.col())?;
        self.db.delete_cf(col, key.encode()?)?;
        Ok(())
    }

    fn get<V: DBValue>(&self, key: K) -> Result<Option<V>, DBError> {
        let col = self.col(key.col())?;
        let raw = self.db.get_pinned_cf(col, key.encode()?)?;
        Ok(match raw {
            Some(raw) => Some(V::decode(&raw[..])?),
            None => None,
        })
    }

    fn write_batch(&self, batch: Batch<K>) -> Result<(), DBError> {
        let mut write_batch = WriteBatch::default();
        for operation in batch.operations {
            match operation {
                Operation::Insert(key, value) => {
                    let col = self.col(key.col())?;
                    write_batch.put_cf(col, key.encode()?, value);
                }
                Operation::Remove(key) => {
                    let col = self.col(key.col())?;
                    write_batch.delete_cf(col, key.encode()?);
                }
            }
        }
        self.db.write(write_batch)?;
        Ok(())
    }

    fn has(&self, key: K) -> Result<bool, DBError> {
        let col = self.col(key.col())?;
        let value = self.db.get_pinned_cf(col, key.encode()?)?;
        Ok(value.is_some())
    }
}
