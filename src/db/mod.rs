mod batch;
mod disk;
mod key;
mod value;

use crate::util::EmptyResult;
pub use batch::Batch;
pub use disk::DiskDatabase;
pub use disk::{
    Iter, IterDirection, IterMode, COL_BLOCKSTORE_BLOCK_RECORD, COL_BLOCKSTORE_FILE,
    COL_BLOCKSTORE_LAST_FILE, COL_CHAIN_ENTRY, COL_CHAIN_ENTRY_HASH, COL_CHAIN_ENTRY_HEIGHT,
    COL_CHAIN_NEXT_HASHES, COL_CHAIN_SKIP, COL_CHAIN_WORK, COL_COIN, COL_MISC, COL_NEXT_HASH,
    COL_VERSION_BIT_STATE, KEY_CHAIN_STATE, KEY_TIP,
};
use failure::Error;
pub use key::Key;
pub use value::DBValue;

pub trait Database {
    fn insert<V: DBValue>(&self, key: Key, value: &V) -> EmptyResult;
    fn remove(&self, key: Key) -> EmptyResult;
    fn get<V: DBValue>(&self, key: Key) -> Result<Option<V>, Error>;
    fn write_batch(&self, batch: Batch) -> EmptyResult;
    fn has(&self, key: Key) -> Result<bool, Error>;
}
