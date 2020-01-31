use super::{
    COL_BLOCKSTORE_BLOCK_RECORD, COL_BLOCKSTORE_FILE, COL_BLOCKSTORE_LAST_FILE, COL_CHAIN_ENTRY,
    COL_CHAIN_ENTRY_HASH, COL_CHAIN_ENTRY_HEIGHT, COL_CHAIN_NEXT_HASHES, COL_CHAIN_SKIP,
    COL_CHAIN_WORK, COL_COIN, COL_MISC, COL_NEXT_HASH, COL_VERSION_BIT_STATE, KEY_CHAIN_STATE,
    KEY_TIP,
};
use crate::blockstore::RecordType;
use bitcoin::{
    consensus::encode::{serialize, Encodable},
    util::uint::Uint256,
    BlockHash, OutPoint,
};
use failure::Error;
use std::io::Cursor;

pub enum Key {
    ChainEntry(BlockHash),
    Coin(OutPoint),
    ChainEntryHash(u32),
    ChainEntryHeight(BlockHash),
    ChainNextHash(BlockHash),
    ChainNextHashes(BlockHash),
    ChainState,
    ChainTip,
    ChainWork(Uint256, BlockHash),
    ChainSkip(BlockHash),
    BlockStoreLastFile(RecordType),
    BlockStoreFile(RecordType, u32),
    BlockStoreBlockRecord(RecordType, BlockHash),
    VersionBitState(u8, BlockHash),
}

impl Key {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        Ok(match self {
            Key::Coin(outpoint) => serialize(outpoint),
            Key::ChainEntryHash(height) => serialize(height),
            Key::ChainEntry(hash)
            | Key::ChainEntryHeight(hash)
            | Key::ChainNextHash(hash)
            | Key::ChainNextHashes(hash)
            | Key::ChainSkip(hash) => serialize(hash),
            Key::ChainTip => KEY_TIP.to_vec(),
            Key::ChainState => KEY_CHAIN_STATE.to_vec(),
            Key::ChainWork(work, hash) => {
                let mut encoder = Cursor::new(Vec::with_capacity(64));
                work.consensus_encode(&mut encoder)?;
                hash.consensus_encode(&mut encoder)?;
                encoder.into_inner()
            }
            Key::BlockStoreBlockRecord(record_type, hash) => {
                let mut encoder = Cursor::new(Vec::with_capacity(36));
                record_type.as_u32().consensus_encode(&mut encoder)?;
                hash.consensus_encode(&mut encoder)?;
                encoder.into_inner()
            }
            Key::BlockStoreLastFile(record_type) => serialize(&record_type.as_u32()),
            Key::BlockStoreFile(record_type, fileno) => {
                let mut encoder = Cursor::new(Vec::with_capacity(36));
                record_type.as_u32().consensus_encode(&mut encoder)?;
                fileno.consensus_encode(&mut encoder)?;
                encoder.into_inner()
            }
            Key::VersionBitState(bit, hash) => {
                let mut encoder = Cursor::new(Vec::with_capacity(36));
                bit.consensus_encode(&mut encoder)?;
                hash.consensus_encode(&mut encoder)?;
                encoder.into_inner()
            }
        })
    }

    pub fn col(&self) -> &'static str {
        use Key::*;
        match self {
            ChainEntry(_) => COL_CHAIN_ENTRY,
            Coin(_) => COL_COIN,
            ChainEntryHash(_) => COL_CHAIN_ENTRY_HASH,
            ChainEntryHeight(_) => COL_CHAIN_ENTRY_HEIGHT,
            ChainNextHash(_) => COL_NEXT_HASH,
            ChainState | ChainTip => COL_MISC,
            ChainWork(_, _) => COL_CHAIN_WORK,
            ChainNextHashes(_) => COL_CHAIN_NEXT_HASHES,
            ChainSkip(_) => COL_CHAIN_SKIP,
            BlockStoreLastFile(_) => COL_BLOCKSTORE_LAST_FILE,
            BlockStoreBlockRecord(_, _) => COL_BLOCKSTORE_BLOCK_RECORD,
            BlockStoreFile(_, _) => COL_BLOCKSTORE_FILE,
            VersionBitState(_, _) => COL_VERSION_BIT_STATE,
        }
    }
}
