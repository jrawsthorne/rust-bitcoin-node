use bitcoin::blockdata::transaction::{Transaction, TxOut};

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
