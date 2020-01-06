use super::{CoinEntry, Coins};
use crate::blockchain::ChainDB;
use crate::util::EmptyResult;
use bitcoin::{hashes::sha256d::Hash as H256, BitcoinHash, OutPoint, Transaction, TxOut};
use failure::{bail, Error};
use std::collections::{hash_map::Entry, HashMap};

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

    pub fn get_output(&self, prevout: &OutPoint) -> Option<&TxOut> {
        let coins = self.get(prevout.txid)?;
        coins.get_output(prevout.vout)
    }

    pub fn get_entry(&self, prevout: &OutPoint) -> Option<&CoinEntry> {
        let coins = self.get(prevout.txid)?;
        coins.get(prevout.vout)
    }

    /// Get a coin from the coin view or from the database if it exists
    pub fn read_coin(
        &mut self,
        db: &mut ChainDB,
        prevout: &OutPoint,
    ) -> Result<Option<&mut CoinEntry>, Error> {
        Ok(match self.map.entry(prevout.txid) {
            Entry::Occupied(entry) => match entry.into_mut().outputs.entry(prevout.vout) {
                Entry::Occupied(entry) => Some(entry.into_mut()),
                Entry::Vacant(entry) => match db.read_coin(prevout)? {
                    Some(coin) => Some(entry.insert(coin.clone())),
                    None => None,
                },
            },
            Entry::Vacant(entry) => match db.read_coin(prevout)? {
                Some(coin) => {
                    let mut coins = Coins::default();
                    coins.add(prevout.vout, coin.clone());
                    entry.insert(coins).get_mut(prevout.vout)
                }
                None => None,
            },
        })
    }

    /// Get every unspent output for the inputs of a transaction
    /// and ensure that the output exists and was not spent in a previous input
    pub fn spend_inputs(&mut self, db: &mut ChainDB, tx: &Transaction) -> EmptyResult {
        for input in &tx.input {
            let coin = self.read_coin(db, &input.previous_output)?;
            match coin {
                Some(coin) => {
                    if coin.spent {
                        bail!("Coin spent");
                    }
                    coin.spent = true;
                }
                None => {
                    bail!("Coin spent or didn't exist");
                }
            }
        }
        Ok(())
    }
}
