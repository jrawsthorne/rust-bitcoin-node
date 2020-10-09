use crate::blockchain::{ChainEntry, ChainState};
use crate::blockstore::{BlockRecord, FileRecord};
use crate::coins::{CoinEntry, UndoCoins};
use crate::protocol::{BIP8ThresholdState, BIP9ThresholdState};
use bitcoin::{
    consensus::{encode, Decodable, Encodable},
    util::uint::Uint256,
    BlockHash, TxMerkleNode, TxOut,
};

pub trait DBValue: Sized {
    fn decode<R: std::io::Read>(decoder: R) -> Result<Self, encode::Error>;
    fn encode(&self) -> Result<Vec<u8>, encode::Error>;
}

impl<T: Decodable + Encodable> DBValue for T {
    fn decode<R: std::io::Read>(decoder: R) -> Result<T, encode::Error> {
        Ok(T::consensus_decode(decoder)?)
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        let mut encoder = Vec::new();
        T::consensus_encode(self, &mut encoder)?;
        Ok(encoder)
    }
}

impl DBValue for BIP9ThresholdState {
    fn decode<R: std::io::Read>(decoder: R) -> Result<Self, encode::Error> {
        let flag = u8::consensus_decode(decoder)?;
        Ok(match flag {
            0 => BIP9ThresholdState::Defined,
            1 => BIP9ThresholdState::Started,
            2 => BIP9ThresholdState::LockedIn,
            3 => BIP9ThresholdState::Active,
            4 => BIP9ThresholdState::Failed,
            _ => unreachable!(),
        })
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        Ok(vec![match self {
            BIP9ThresholdState::Defined => 0,
            BIP9ThresholdState::Started => 1,
            BIP9ThresholdState::LockedIn => 2,
            BIP9ThresholdState::Active => 3,
            BIP9ThresholdState::Failed => 4,
        }])
    }
}

impl DBValue for BIP8ThresholdState {
    fn decode<R: std::io::Read>(decoder: R) -> Result<Self, encode::Error> {
        let flag = u8::consensus_decode(decoder)?;
        Ok(match flag {
            0 => BIP8ThresholdState::Defined,
            1 => BIP8ThresholdState::Started,
            2 => BIP8ThresholdState::LockedIn,
            3 => BIP8ThresholdState::Active,
            4 => BIP8ThresholdState::Failing,
            5 => BIP8ThresholdState::Failed,
            _ => unreachable!(),
        })
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        Ok(vec![match self {
            BIP8ThresholdState::Defined => 0,
            BIP8ThresholdState::Started => 1,
            BIP8ThresholdState::LockedIn => 2,
            BIP8ThresholdState::Active => 3,
            BIP8ThresholdState::Failing => 4,
            BIP8ThresholdState::Failed => 5,
        }])
    }
}

impl DBValue for ChainEntry {
    fn decode<R: std::io::Read>(mut decoder: R) -> Result<Self, encode::Error> {
        let hash = BlockHash::consensus_decode(&mut decoder)?;
        let version = i32::consensus_decode(&mut decoder)?;
        let prev_block = BlockHash::consensus_decode(&mut decoder)?;
        let merkle_root = TxMerkleNode::consensus_decode(&mut decoder)?;
        let time = u32::consensus_decode(&mut decoder)?;
        let bits = u32::consensus_decode(&mut decoder)?;
        let nonce = u32::consensus_decode(&mut decoder)?;
        let height = u32::consensus_decode(&mut decoder)?;
        let chainwork = Uint256::consensus_decode(&mut decoder)?;
        let skip = BlockHash::consensus_decode(&mut decoder)?;
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
            skip,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        let mut encoder = Vec::with_capacity(170);
        self.hash.consensus_encode(&mut encoder)?;
        self.version.consensus_encode(&mut encoder)?;
        self.prev_block.consensus_encode(&mut encoder)?;
        self.merkle_root.consensus_encode(&mut encoder)?;
        self.time.consensus_encode(&mut encoder)?;
        self.bits.consensus_encode(&mut encoder)?;
        self.nonce.consensus_encode(&mut encoder)?;
        self.height.consensus_encode(&mut encoder)?;
        self.chainwork.consensus_encode(&mut encoder)?;
        self.skip.consensus_encode(&mut encoder)?;
        Ok(encoder)
    }
}

pub static MAX_HEIGHT: u32 = u32::max_value();

// TODO: Compress
impl DBValue for CoinEntry {
    fn decode<R: std::io::Read>(mut decoder: R) -> Result<Self, encode::Error> {
        let version = i32::consensus_decode(&mut decoder)?;
        let height = match u32::consensus_decode(&mut decoder)? {
            height if height == MAX_HEIGHT => None,
            height => Some(height),
        };
        let coinbase = bool::consensus_decode(&mut decoder)?;
        let output = TxOut::consensus_decode(&mut decoder)?;
        // TODO: Don't store spent. If it still exists, it can't have been spent
        let _spent = bool::consensus_decode(&mut decoder)?;
        Ok(CoinEntry {
            version,
            height,
            coinbase,
            output,
            spent: false,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        // version + height + coinbase + output value + output script + spent
        let len = 4 + 4 + 1 + 8 + self.output.script_pubkey.len() + 1;
        let mut encoder = Vec::with_capacity(len);
        self.version.consensus_encode(&mut encoder)?;
        match self.height {
            Some(height) => height.consensus_encode(&mut encoder)?,
            None => MAX_HEIGHT.consensus_encode(&mut encoder)?,
        };
        self.coinbase.consensus_encode(&mut encoder)?;
        self.output.consensus_encode(&mut encoder)?;
        // TODO: Don't store spent. If it still exists, it can't have been spent
        self.spent.consensus_encode(&mut encoder)?;
        Ok(encoder)
    }
}

impl DBValue for UndoCoins {
    fn decode<R: std::io::Read>(mut decoder: R) -> Result<Self, encode::Error> {
        let count = u32::consensus_decode(&mut decoder)?;

        let mut items = Vec::with_capacity(count as usize);

        for _ in 0..count {
            items.push(CoinEntry::decode(&mut decoder)?);
        }

        Ok(UndoCoins { items })
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        let count = self.items.len();
        let mut encoder = Vec::new();

        (count as u32).consensus_encode(&mut encoder)?;

        // TODO encode with io write

        for coin in &self.items {
            encoder.extend(coin.encode()?);
        }

        Ok(encoder)
    }
}

impl DBValue for ChainState {
    fn decode<R: std::io::Read>(mut decoder: R) -> Result<Self, encode::Error> {
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

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        // length = tip + tx + coin + value
        let mut encoder = Vec::with_capacity(32 + 8 + 8 + 8);
        self.tip.consensus_encode(&mut encoder)?;
        self.tx.consensus_encode(&mut encoder)?;
        self.coin.consensus_encode(&mut encoder)?;
        self.value.consensus_encode(&mut encoder)?;
        Ok(encoder)
    }
}

impl DBValue for FileRecord {
    fn decode<R: std::io::Read>(mut decoder: R) -> Result<Self, encode::Error> {
        let blocks = u32::consensus_decode(&mut decoder)?;
        let used = u32::consensus_decode(&mut decoder)?;
        let length = u32::consensus_decode(&mut decoder)?;
        Ok(FileRecord {
            blocks,
            used,
            length,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        let mut encoder = Vec::with_capacity(12);
        self.blocks.consensus_encode(&mut encoder)?;
        self.used.consensus_encode(&mut encoder)?;
        self.length.consensus_encode(&mut encoder)?;
        Ok(encoder)
    }
}

impl DBValue for BlockRecord {
    fn decode<R: std::io::Read>(mut decoder: R) -> Result<Self, encode::Error> {
        let file = u32::consensus_decode(&mut decoder)?;
        let position = u32::consensus_decode(&mut decoder)?;
        let length = u32::consensus_decode(&mut decoder)?;
        Ok(BlockRecord {
            file,
            position,
            length,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        let mut encoder = Vec::with_capacity(12);
        self.file.consensus_encode(&mut encoder)?;
        self.position.consensus_encode(&mut encoder)?;
        self.length.consensus_encode(&mut encoder)?;
        Ok(encoder)
    }
}
