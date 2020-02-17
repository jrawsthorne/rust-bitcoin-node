use crate::blockchain::Chain;
use crate::coins::CoinView;
use crate::net::PeerId;
use bitcoin::{Block, Transaction};
use bitcoin::{OutPoint, Txid};
use failure::Error;
use log::debug;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Trait that handles mempool events
pub trait MempoolListener {
    fn handle_tx(&self, _tx: &Transaction, _view: &CoinView) {}
    fn handle_bad_orphan(&self, _error: &Error, _peer_id: PeerId) {}
    fn handle_confirmed(&self, _tx: &Transaction, _block: &Block) {}
    fn handle_error(&self, _error: &Error) {}
    fn handle_unconfirmed(&self, _tx: &Transaction, _block: &Block) {}
    fn handle_conflict(&self, _tx: &Transaction) {}
    fn handle_add_entry(&self, _entry: &MempoolEntry) {}
    fn handle_remove_entry(&self, _entry: &MempoolEntry) {}
    fn handle_add_orphan(&self, _tx: &Transaction) {}
    fn handle_remove_orphan(&self, _tx: &Transaction) {}
    fn handle_double_spend(&self, _spent: &MempoolEntry) {}
}

pub struct MempoolEntry {}

pub struct TxMemPoolEntry {
    /// the transaction
    tx: Transaction,
    /// height of chain when transaction was added to pool
    height: u32,
}

impl TxMemPoolEntry {
    fn new(tx: Transaction, height: u32) -> Self {
        Self { tx, height }
    }
}

pub struct TxMemPool {
    /// transactions stored in the mempool
    transactions: HashMap<Txid, TxMemPoolEntry>,
    /// map of outpoint to the txid of the transaction that spent that output
    spents: HashMap<OutPoint, Txid>,
    chain: Arc<Mutex<Chain>>,
}

impl TxMemPool {
    pub fn new(chain: Arc<Mutex<Chain>>) -> Self {
        Self {
            transactions: Default::default(),
            spents: Default::default(),
            chain,
        }
    }

    /// check if tx is double spend from the viewpoint of the mempool
    /// doesn't check if chain double spend
    fn is_double_spend(&self, tx: &Transaction) -> bool {
        for input in &tx.input {
            if self.spents.contains_key(&input.previous_output) {
                return true;
            }
        }
        false
    }

    /// add a transaction
    pub async fn add_tx(&mut self, tx: Transaction) {
        let hash = tx.txid();
        if self.transactions.contains_key(&hash) {
            return;
        }
        if self.is_double_spend(&tx) {
            return;
        }
        for input in &tx.input {
            self.spents.insert(input.previous_output, hash);
        }
        let height = self.chain.lock().await.height;

        self.transactions
            .insert(hash, TxMemPoolEntry::new(tx, height));

        debug!(
            "Added {} to mempool (txs={},spents={}).",
            hash,
            self.transactions.len(),
            self.spents.len()
        );
    }

    pub fn add_block(&mut self, txs: &[Transaction], height: u32) {
        // we don't need to remove any transactions
        // if there are none in the mempool
        if self.transactions.is_empty() {
            return;
        }

        let mut entries = vec![];

        // remove all transactions that were in the block from the mempool
        // as well as any transactions that spend any inputs from transactions
        // in the block as they are now double spends
        for tx in txs {
            let hash = tx.txid();
            if let Some(entry) = self.transactions.remove(&hash) {
                for input in &entry.tx.input {
                    self.spents.remove(&input.previous_output);
                }
                entries.push(entry);
            } else {
                self.remove_double_spends(tx);
            }
        }

        debug!(
            "Removed {} txs from mempool for block {}.",
            entries.len(),
            height
        );
    }

    // remove all of the inputs this transaction spends from spents
    // remove any transactions that spend those inputs
    // recursively remove any transactions that spend the outputs
    pub fn remove_double_spends(&mut self, tx: &Transaction) {
        for input in &tx.input {
            if let Some(double_spend) = self.remove_spent(&input.previous_output) {
                self.remove_spenders(&double_spend);
            }
        }
    }

    // is it ok to remove before recursing?
    fn remove_spenders(&mut self, entry: &TxMemPoolEntry) {
        let tx = &entry.tx;
        let txid = tx.txid();

        for i in 0..tx.output.len() {
            let out_point = OutPoint {
                vout: i as u32,
                txid,
            };
            if let Some(spender) = self.remove_spent(&out_point) {
                self.remove_spenders(&spender);
            }
        }
    }

    fn get_spent(&self, out_point: &OutPoint) -> Option<&TxMemPoolEntry> {
        let txid = self.spents.get(out_point)?;
        self.transactions.get(txid)
    }

    fn remove_spent(&mut self, out_point: &OutPoint) -> Option<TxMemPoolEntry> {
        let txid = self.spents.remove(out_point)?;
        self.transactions.remove(&txid)
    }
}
