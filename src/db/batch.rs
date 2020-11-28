use super::DBKey;
use bitcoin::consensus::{serialize, Encodable};

pub struct Batch<K: DBKey> {
    pub operations: Vec<Operation<K>>,
}

impl<K: DBKey> Batch<K> {
    pub fn new() -> Self {
        Self { operations: vec![] }
    }

    pub fn insert<V: Encodable>(&mut self, key: K, value: &V) {
        self.operations
            .push(Operation::Insert(key, serialize(value)));
    }

    pub fn remove(&mut self, key: K) {
        self.operations.push(Operation::Remove(key));
    }
}

pub enum Operation<K> {
    Insert(K, Vec<u8>),
    Remove(K),
}
