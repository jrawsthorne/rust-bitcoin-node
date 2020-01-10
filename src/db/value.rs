use crate::blockchain::{ChainEntry, ChainState};
use crate::coins::CoinEntry;
use bitcoin::{
    consensus::{Decodable, Encodable},
    util::uint::Uint256,
    BlockHash, TxMerkleNode, TxOut,
};
use failure::Error;
use std::io::Cursor;

pub trait DBValue: Sized {
    fn decode(bytes: &[u8]) -> Result<Self, Error>;
    fn encode(&self) -> Result<Vec<u8>, Error>;
}

impl<T: Decodable + Encodable> DBValue for T {
    fn decode(bytes: &[u8]) -> Result<T, Error> {
        let decoder = Cursor::new(bytes);
        Ok(T::consensus_decode(decoder)?)
    }
    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut encoder = Cursor::new(Vec::new());
        T::consensus_encode(self, &mut encoder)?;
        Ok(encoder.into_inner())
    }
}

impl DBValue for ChainEntry {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut decoder = Cursor::new(bytes);
        let mut entry = ChainEntry::default();
        entry.hash = BlockHash::consensus_decode(&mut decoder)?;
        entry.version = u32::consensus_decode(&mut decoder)?;
        entry.prev_block = BlockHash::consensus_decode(&mut decoder)?;
        entry.merkle_root = TxMerkleNode::consensus_decode(&mut decoder)?;
        entry.time = u32::consensus_decode(&mut decoder)?;
        entry.bits = u32::consensus_decode(&mut decoder)?;
        entry.nonce = u32::consensus_decode(&mut decoder)?;
        entry.height = u32::consensus_decode(&mut decoder)?;
        entry.chainwork = Uint256::consensus_decode(&mut decoder)?;
        Ok(entry)
    }
    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut encoder = Cursor::new(Vec::with_capacity(148));
        self.hash.consensus_encode(&mut encoder)?;
        self.version.consensus_encode(&mut encoder)?;
        self.prev_block.consensus_encode(&mut encoder)?;
        self.merkle_root.consensus_encode(&mut encoder)?;
        self.time.consensus_encode(&mut encoder)?;
        self.bits.consensus_encode(&mut encoder)?;
        self.nonce.consensus_encode(&mut encoder)?;
        self.height.consensus_encode(&mut encoder)?;
        self.chainwork.consensus_encode(&mut encoder)?;
        Ok(encoder.into_inner())
    }
}

pub static MAX_HEIGHT: u32 = u32::max_value();

// TODO: Compress
impl DBValue for CoinEntry {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut decoder = Cursor::new(bytes);
        let mut entry = CoinEntry::default();
        entry.version = u32::consensus_decode(&mut decoder)?;
        entry.height = match u32::consensus_decode(&mut decoder)? {
            height if height == MAX_HEIGHT => None,
            height => Some(height),
        };
        entry.coinbase = bool::consensus_decode(&mut decoder)?;
        entry.output = TxOut::consensus_decode(&mut decoder)?;
        entry.spent = bool::consensus_decode(&mut decoder)?;
        Ok(entry)
    }
    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut encoder = Cursor::new(Vec::new());
        self.version.consensus_encode(&mut encoder)?;
        match self.height {
            Some(height) => height.consensus_encode(&mut encoder)?,
            None => MAX_HEIGHT.consensus_encode(&mut encoder)?,
        };
        self.coinbase.consensus_encode(&mut encoder)?;
        self.output.consensus_encode(&mut encoder)?;
        self.spent.consensus_encode(&mut encoder)?;
        Ok(encoder.into_inner())
    }
}

impl DBValue for ChainState {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut decoder = Cursor::new(bytes);
        let mut state = ChainState::default();
        state.tip = BlockHash::consensus_decode(&mut decoder)?;
        state.tx = u32::consensus_decode(&mut decoder)? as usize;
        state.coin = u32::consensus_decode(&mut decoder)? as usize;
        state.value = u64::consensus_decode(&mut decoder)?;
        Ok(state)
    }
    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut encoder = Cursor::new(Vec::new());
        self.tip.consensus_encode(&mut encoder)?;
        (self.tx as u32).consensus_encode(&mut encoder)?;
        (self.coin as u32).consensus_encode(&mut encoder)?;
        self.value.consensus_encode(&mut encoder)?;
        Ok(encoder.into_inner())
    }
}
