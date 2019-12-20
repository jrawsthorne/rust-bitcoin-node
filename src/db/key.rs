use super::{
    COL_CHAIN_ENTRY, COL_CHAIN_ENTRY_HASH, COL_CHAIN_ENTRY_HEIGHT, COL_COIN, COL_MISC,
    COL_NEXT_HASH, KEY_CHAIN_STATE, KEY_TIP,
};
use bitcoin::{
    consensus::encode::{serialize, Encodable},
    hashes::sha256d::Hash as H256,
};
use failure::Error;
use std::io::Cursor;

pub enum Key {
    ChainEntry(H256),
    Coin(H256, u32),
    ChainEntryHash(u32),
    ChainEntryHeight(H256),
    ChainNextHash(H256),
    ChainState,
    ChainTip,
}

impl Key {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        Ok(match self {
            Key::Coin(hash, index) => {
                let mut encoder = Cursor::new(Vec::new());
                hash.consensus_encode(&mut encoder)?;
                index.consensus_encode(&mut encoder)?;
                encoder.into_inner()
            }
            Key::ChainEntryHash(height) => serialize(height),
            Key::ChainEntry(hash) | Key::ChainEntryHeight(hash) | Key::ChainNextHash(hash) => {
                serialize(hash)
            }
            Key::ChainTip => KEY_TIP.to_vec(),
            Key::ChainState => KEY_CHAIN_STATE.to_vec(),
        })
    }

    pub fn col(&self) -> &'static str {
        match self {
            Key::ChainEntry(_) => COL_CHAIN_ENTRY,
            Key::Coin(_, _) => COL_COIN,
            Key::ChainEntryHash(_) => COL_CHAIN_ENTRY_HASH,
            Key::ChainEntryHeight(_) => COL_CHAIN_ENTRY_HEIGHT,
            Key::ChainNextHash(_) => COL_NEXT_HASH,
            Key::ChainState | Key::ChainTip => COL_MISC,
        }
    }
}