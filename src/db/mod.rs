mod batch;
mod disk;
mod key;
mod value;

use crate::error::DBError;
pub use batch::Batch;
pub use disk::DiskDatabase;
pub use disk::{Iter, IterDirection, IterMode, KEY_CHAIN_STATE, KEY_TIP};
pub use key::DBKey;
pub use value::DBValue;

pub trait Database<K: DBKey> {
    fn insert(&self, key: K, value: &impl DBValue) -> Result<(), DBError>;
    fn remove(&self, key: K) -> Result<(), DBError>;
    fn get<V: DBValue>(&self, key: K) -> Result<Option<V>, DBError>;
    fn write_batch(&self, batch: Batch<K>) -> Result<(), DBError>;
    fn has(&self, key: K) -> Result<bool, DBError>;
}
