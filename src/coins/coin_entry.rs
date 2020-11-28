use bitcoin::{
    blockdata::transaction::{Transaction, TxOut},
    consensus::{encode, Decodable, Encodable},
};

/// A single unspent transaction output or coin
#[derive(Debug, Clone)]
pub struct CoinEntry {
    /// The transaction version
    pub version: i32,
    /// The height of the block this output was created in
    pub height: Option<u32>,
    /// Whether this coin originated from a coinbase transaction.
    /// Used to check that a coinbase is not spent until after 100 blocks have been mined.
    pub coinbase: bool,
    /// The transaction output
    pub output: TxOut,
    /// Whether this coin has been spent.
    /// When verifying a block, once this has been set to true, another attempt
    /// at spending this coin will fail as it will be a double spend
    pub spent: bool,
}

impl Default for CoinEntry {
    fn default() -> Self {
        Self {
            version: 1,
            height: None,
            coinbase: false,
            output: TxOut::default(),
            spent: false,
        }
    }
}

impl CoinEntry {
    /// Create a coin entry from a transaction output at the given chain height
    pub fn from_tx(tx: &Transaction, index: u32, height: u32) -> Self {
        Self {
            version: tx.version,
            height: Some(height),
            coinbase: tx.is_coin_base(),
            output: tx.output[index as usize].clone(),
            spent: false,
        }
    }
}

pub static MAX_HEIGHT: u32 = u32::max_value();

impl Encodable for CoinEntry {
    fn consensus_encode<W: std::io::Write>(&self, mut e: W) -> Result<usize, encode::Error> {
        let mut len = 0;
        len += self.version.consensus_encode(&mut e)?;
        len += match self.height {
            Some(height) => height.consensus_encode(&mut e),
            None => MAX_HEIGHT.consensus_encode(&mut e),
        }?;
        len += self.coinbase.consensus_encode(&mut e)?;
        len += self.output.consensus_encode(&mut e)?;
        // TODO: Don't store spent. If it still exists, it can't have been spent
        len += self.spent.consensus_encode(&mut e)?;
        Ok(len)
    }
}

impl Decodable for CoinEntry {
    fn consensus_decode<D: std::io::Read>(mut d: D) -> Result<Self, encode::Error> {
        let version = i32::consensus_decode(&mut d)?;
        let height = match u32::consensus_decode(&mut d)? {
            height if height == MAX_HEIGHT => None,
            height => Some(height),
        };
        let coinbase = bool::consensus_decode(&mut d)?;
        let output = TxOut::consensus_decode(&mut d)?;
        // TODO: Don't store spent. If it still exists, it can't have been spent
        let _spent = bool::consensus_decode(&mut d)?;
        Ok(CoinEntry {
            version,
            height,
            coinbase,
            output,
            spent: false,
        })
    }
}
