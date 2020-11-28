mod batch;
mod disk;
mod key;

use crate::error::DBError;
pub use batch::Batch;
use bitcoin::consensus::Decodable;
pub use disk::DiskDatabase;
pub use disk::{Iter, IterDirection, IterMode, KEY_CHAIN_STATE, KEY_TIP};
pub use key::DBKey;

pub trait Database {
    fn get<K: DBKey, V: Decodable>(&self, key: K) -> Result<Option<V>, DBError>;
    fn write_batch<K: DBKey>(&self, batch: Batch<K>) -> Result<(), DBError>;
    fn has<K: DBKey>(&self, key: K) -> Result<bool, DBError>;
}
