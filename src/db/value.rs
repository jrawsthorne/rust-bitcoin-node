use crate::blockchain::{ChainEntry, ChainState};
use crate::blockstore::{BlockRecord, FileRecord};
use crate::coins::CoinEntry;
use crate::protocol::ThresholdState;
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

impl DBValue for ThresholdState {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Ok(match bytes[0] {
            0 => ThresholdState::Defined,
            1 => ThresholdState::Started,
            2 => ThresholdState::LockedIn,
            3 => ThresholdState::Active,
            4 => ThresholdState::Failed,
            _ => failure::bail!("bad state"),
        })
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        Ok(vec![match self {
            ThresholdState::Defined => 0,
            ThresholdState::Started => 1,
            ThresholdState::LockedIn => 2,
            ThresholdState::Active => 3,
            ThresholdState::Failed => 4,
        }])
    }
}

impl DBValue for ChainEntry {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut decoder = Cursor::new(bytes);
        let hash = BlockHash::consensus_decode(&mut decoder)?;
        let version = u32::consensus_decode(&mut decoder)?;
        let prev_block = BlockHash::consensus_decode(&mut decoder)?;
        let merkle_root = TxMerkleNode::consensus_decode(&mut decoder)?;
        let time = u32::consensus_decode(&mut decoder)?;
        let bits = u32::consensus_decode(&mut decoder)?;
        let nonce = u32::consensus_decode(&mut decoder)?;
        let height = u32::consensus_decode(&mut decoder)?;
        let chainwork = Uint256::consensus_decode(&mut decoder)?;
        Ok(ChainEntry {
            hash,
            version,
            prev_block,
            merkle_root,
            time,
            bits,
            nonce,
            height,
            chainwork,
        })
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
        let version = u32::consensus_decode(&mut decoder)?;
        let height = match u32::consensus_decode(&mut decoder)? {
            height if height == MAX_HEIGHT => None,
            height => Some(height),
        };
        let coinbase = bool::consensus_decode(&mut decoder)?;
        let output = TxOut::consensus_decode(&mut decoder)?;
        let spent = bool::consensus_decode(&mut decoder)?;
        Ok(CoinEntry {
            version,
            height,
            coinbase,
            output,
            spent,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        // version + height + coinbase + output value + output script + spent
        let len = 4 + 4 + 1 + 8 + self.output.script_pubkey.len() + 1;
        let mut encoder = Cursor::new(Vec::with_capacity(len));
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
        let tip = BlockHash::consensus_decode(&mut decoder)?;
        let tx = u64::consensus_decode(&mut decoder)?;
        let coin = u64::consensus_decode(&mut decoder)?;
        let value = u64::consensus_decode(&mut decoder)?;
        Ok(ChainState {
            tip,
            tx,
            coin,
            value,
            commited: false,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        // length = tip + tx + coin + value
        let mut encoder = Cursor::new(Vec::with_capacity(32 + 8 + 8 + 8));
        self.tip.consensus_encode(&mut encoder)?;
        self.tx.consensus_encode(&mut encoder)?;
        self.coin.consensus_encode(&mut encoder)?;
        self.value.consensus_encode(&mut encoder)?;
        Ok(encoder.into_inner())
    }
}

impl DBValue for FileRecord {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut decoder = Cursor::new(bytes);
        let blocks = u32::consensus_decode(&mut decoder)?;
        let used = u32::consensus_decode(&mut decoder)?;
        let length = u32::consensus_decode(&mut decoder)?;
        Ok(FileRecord {
            blocks,
            used,
            length,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut encoder = Cursor::new(Vec::with_capacity(12));
        self.blocks.consensus_encode(&mut encoder)?;
        self.used.consensus_encode(&mut encoder)?;
        self.length.consensus_encode(&mut encoder)?;
        Ok(encoder.into_inner())
    }
}

impl DBValue for BlockRecord {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut decoder = Cursor::new(bytes);
        let file = u32::consensus_decode(&mut decoder)?;
        let position = u32::consensus_decode(&mut decoder)?;
        let length = u32::consensus_decode(&mut decoder)?;
        Ok(BlockRecord {
            file,
            position,
            length,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut encoder = Cursor::new(Vec::with_capacity(12));
        self.file.consensus_encode(&mut encoder)?;
        self.position.consensus_encode(&mut encoder)?;
        self.length.consensus_encode(&mut encoder)?;
        Ok(encoder.into_inner())
    }
}
