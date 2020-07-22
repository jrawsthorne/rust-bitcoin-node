use super::{DBKey, DBValue};
use bitcoin::consensus::encode;

pub struct Batch<K: DBKey> {
    pub operations: Vec<Operation<K>>,
}

impl<K: DBKey> Batch<K> {
    pub fn new() -> Self {
        Self { operations: vec![] }
    }

    pub fn insert<V: DBValue>(&mut self, key: K, value: &V) -> Result<(), encode::Error> {
        self.operations
            .push(Operation::Insert(key, value.encode()?));
        Ok(())
    }
    pub fn remove(&mut self, key: K) {
        self.operations.push(Operation::Remove(key));
    }
}

pub enum Operation<K: DBKey> {
    Insert(K, Vec<u8>),
    Remove(K),
}
