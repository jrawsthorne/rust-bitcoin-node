mod batch;
mod disk;
mod key;
mod value;

use crate::util::EmptyResult;
pub use batch::Batch;
pub use disk::DiskDatabase;
pub use disk::{
    COL_CHAIN_ENTRY, COL_CHAIN_ENTRY_HASH, COL_CHAIN_ENTRY_HEIGHT, COL_COIN, COL_MISC,
    COL_NEXT_HASH, KEY_CHAIN_STATE, KEY_TIP,
};
use failure::Error;
pub use key::Key;
pub use value::DBValue;

pub trait Database {
    fn insert<V: DBValue>(&self, key: Key, value: &V) -> EmptyResult;
    fn remove(&self, key: Key) -> EmptyResult;
    fn get<V: DBValue>(&self, key: Key) -> Result<Option<V>, Error>;
    fn write_batch(&self, batch: Batch) -> EmptyResult;
}
