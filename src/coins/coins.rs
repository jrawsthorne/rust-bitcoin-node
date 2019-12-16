use super::CoinEntry;
use bitcoin::{Transaction, TxOut};
use std::collections::HashMap;

/// The set of unspent coins from a transaction
#[derive(Debug, Clone, Default)]
pub struct Coins {
    /// A map of an output index to a coin
    pub outputs: HashMap<u32, CoinEntry>,
}

impl Coins {
    /// Add a coin to the set for an output index
    pub fn add(&mut self, index: u32, coin: CoinEntry) -> &mut CoinEntry {
        self.outputs.entry(index).or_insert(coin)
    }

    /// Check if the set contains a coin for an output index
    pub fn has(&self, index: &u32) -> bool {
        self.outputs.contains_key(index)
    }

    /// Get a reference to an entry for an output index
    pub fn get(&self, index: &u32) -> Option<&CoinEntry> {
        self.outputs.get(index)
    }

    /// Get a mutable reference to an entry for an output index
    pub fn get_mut(&mut self, index: &u32) -> Option<&mut CoinEntry> {
        self.outputs.get_mut(index)
    }

    /// Get the transaction output for an output index if the index exists
    pub fn get_output(&self, index: &u32) -> Option<&TxOut> {
        if let Some(entry) = self.get(index) {
            Some(&entry.output)
        } else {
            None
        }
    }

    /// Create coins from the outputs of a transaction.
    /// Provably unspendable outputs are never added
    /// because they cannot be spent in the future.
    pub fn from_tx(tx: &Transaction, height: u32) -> Coins {
        let mut coins = Coins::default();
        for (i, output) in tx.output.iter().enumerate() {
            if output.script_pubkey.is_provably_unspendable() {
                continue;
            }
            let entry = CoinEntry::from_tx(tx, i as u32, height);
            coins.outputs.insert(i as u32, entry);
        }
        coins
    }
}
