use super::Coins;
use bitcoin::{hashes::sha256d::Hash as H256, BitcoinHash, Transaction};
use std::collections::HashMap;

/// A view of the UTXO set
#[derive(Debug, Clone, Default)]
pub struct CoinView {
    /// A map of transaction ID to coins
    pub map: HashMap<H256, Coins>,
}

impl CoinView {
    /// Get a reference to the coins for a transaction ID
    pub fn get(&self, hash: H256) -> Option<&Coins> {
        self.map.get(&hash)
    }

    /// Get a mutable reference to the coins for a transaction ID
    pub fn get_mut(&mut self, hash: H256) -> Option<&mut Coins> {
        self.map.get_mut(&hash)
    }

    /// Add the coins for a transaction ID
    pub fn add(&mut self, hash: H256, coins: Coins) {
        self.map.insert(hash, coins);
    }

    /// Get the coins for a transaction ID or create an empty set
    pub fn ensure(&mut self, hash: H256) -> &mut Coins {
        self.map.entry(hash).or_default()
    }

    /// Add a new transaction to the view
    pub fn add_tx(&mut self, tx: &Transaction, height: u32) {
        let hash = tx.bitcoin_hash();
        let coins = Coins::from_tx(tx, height);
        self.add(hash, coins)
    }
}
