use super::{
    COL_CHAIN_ENTRY, COL_CHAIN_ENTRY_HASH, COL_CHAIN_ENTRY_HEIGHT, COL_COIN, COL_MISC,
    COL_NEXT_HASH, KEY_CHAIN_STATE, KEY_TIP,
};
use bitcoin::{
    consensus::encode::{serialize, Encodable},
    BlockHash, Txid,
};
use failure::Error;
use std::io::Cursor;

pub enum Key {
    ChainEntry(BlockHash),
    Coin(Txid, u32),
    ChainEntryHash(u32),
    ChainEntryHeight(BlockHash),
    ChainNextHash(BlockHash),
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
        use Key::*;
        match self {
            ChainEntry(_) => COL_CHAIN_ENTRY,
            Coin(_, _) => COL_COIN,
            ChainEntryHash(_) => COL_CHAIN_ENTRY_HASH,
            ChainEntryHeight(_) => COL_CHAIN_ENTRY_HEIGHT,
            ChainNextHash(_) => COL_NEXT_HASH,
            ChainState | Key::ChainTip => COL_MISC,
        }
    }
}
