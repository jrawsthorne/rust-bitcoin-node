use super::{DBValue, Key};
use bitcoin::consensus::encode;

#[derive(Default)]
pub struct Batch {
    pub operations: Vec<Operation>,
}

impl Batch {
    pub fn insert<V: DBValue>(&mut self, key: Key, value: &V) -> Result<(), encode::Error> {
        self.operations
            .push(Operation::Insert(key, value.encode()?));
        Ok(())
    }
    pub fn remove(&mut self, key: Key) {
        self.operations.push(Operation::Remove(key));
    }
}

pub enum Operation {
    Insert(Key, Vec<u8>),
    Remove(Key),
}
