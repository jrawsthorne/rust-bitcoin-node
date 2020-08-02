use super::{CoinEntry, UndoCoins};
use crate::blockchain::ChainDB;
use crate::error::{DBError, TransactionVerificationError};
use bitcoin::{OutPoint, Transaction, TxOut};
use std::collections::{hash_map::Entry, HashMap};

/// A view of the UTXO set
#[derive(Debug, Clone, Default)]
pub struct CoinView {
    /// A map of transaction ID to coins
    pub map: HashMap<OutPoint, CoinEntry>,
    pub undo: UndoCoins,
}

impl CoinView {
    /// Add a new transaction to the view
    pub fn add_tx(&mut self, tx: &Transaction, height: u32) {
        self.index_tx(tx, height, false);
    }

    fn index_tx(&mut self, tx: &Transaction, height: u32, spent: bool) {
        let txid = tx.txid();

        for (index, output) in tx.output.iter().enumerate() {
            if output.script_pubkey.is_provably_unspendable() {
                continue;
            }
            self.map.insert(
                OutPoint {
                    vout: index as u32,
                    txid,
                },
                CoinEntry {
                    version: tx.version,
                    height: Some(height),
                    coinbase: tx.is_coin_base(),
                    output: output.clone(),
                    spent,
                },
            );
        }
    }

    pub fn remove_tx(&mut self, tx: &Transaction, height: u32) {
        self.index_tx(tx, height, true);
    }

    pub fn get_output(&self, prevout: &OutPoint) -> Option<&TxOut> {
        self.map.get(prevout).and_then(|coin| Some(&coin.output))
    }

    pub fn get_entry(&self, prevout: &OutPoint) -> Option<&CoinEntry> {
        self.map.get(prevout)
    }

    /// Get a coin from the coin view or from the database if it exists
    pub fn read_coin(
        &mut self,
        db: &ChainDB,
        prevout: OutPoint,
    ) -> Result<Option<&mut CoinEntry>, DBError> {
        Ok(match self.map.entry(prevout) {
            Entry::Occupied(entry) => Some(entry.into_mut()),
            Entry::Vacant(entry) => db
                .read_coin(prevout)?
                .and_then(|coin| Some(entry.insert(coin.clone()))),
        })
    }

    /// Get every unspent output for the inputs of a transaction
    /// and ensure that the output exists and was not spent in a previous input
    pub fn spend_inputs(
        &mut self,
        db: &ChainDB,
        tx: &Transaction,
    ) -> Result<(), TransactionVerificationError> {
        for input in &tx.input {
            let coin = self.read_coin(db, input.previous_output).unwrap();
            match coin {
                Some(coin) if !coin.spent => {
                    coin.spent = true;
                    let undo_coin = coin.clone();
                    self.undo.push(undo_coin);
                }
                _ => return Err(TransactionVerificationError::InputsMissingOrSpent), // should have already been checked,
            }
        }
        Ok(())
    }
}
