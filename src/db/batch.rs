use super::{DBValue, Key};
use crate::util::EmptyResult;

pub struct Batch {
    pub operations: Vec<Operation>,
}

impl Batch {
    pub fn insert<V: DBValue>(&mut self, key: Key, value: &V) -> EmptyResult {
        self.operations
            .push(Operation::Insert(key, value.encode()?));
        Ok(())
    }
    pub fn remove(&mut self, key: Key) -> EmptyResult {
        self.operations.push(Operation::Remove(key));
        Ok(())
    }
}

pub enum Operation {
    Insert(Key, Vec<u8>),
    Remove(Key),
}
