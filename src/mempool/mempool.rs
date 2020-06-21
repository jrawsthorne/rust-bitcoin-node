use crate::blockchain::Chain;
use crate::coins::CoinView;
use crate::{
    error::TransactionVerificationError,
    protocol::consensus::{LockFlags, ScriptFlags},
    verification::TransactionVerifier,
    ChainEntry, CoinEntry,
};
use bitcoin::Transaction;
use bitcoin::{OutPoint, Txid};
use log::{debug, error, info};
use std::collections::HashMap;
use thiserror::Error;

pub struct MemPoolEntry {
    /// the transaction
    tx: Transaction,
    /// height of chain when transaction was added to pool
    height: u32,
    coinbase: bool,
}

impl MemPoolEntry {
    fn new(tx: Transaction, view: &CoinView, height: u32) -> Self {
        let coinbase = tx
            .input
            .iter()
            .any(|input| view.map[&input.previous_output].coinbase);

        Self {
            tx,
            height,
            coinbase,
        }
    }
}

#[derive(Debug, Error)]
pub enum MempoolError {
    #[error(transparent)]
    TransactionVerificationError(#[from] TransactionVerificationError),
    #[error("coinbase")]
    Coinbase,
    #[error("premature witness")]
    PrematureWitness,
    #[error("premature tx version")]
    PrematureTxVersion,
    #[error("duplicate")]
    Duplicate,
    #[error("double spend")]
    DoubleSpend,
    #[error("orphan")]
    Orphan,
}

#[derive(Default)]
pub struct MemPool {
    /// transactions stored in the mempool
    transactions: HashMap<Txid, MemPoolEntry>,
    /// map of outpoint to the txid of the transaction that spent that output
    spents: HashMap<OutPoint, Txid>,
}

impl MemPool {
    pub fn new() -> Self {
        Self {
            transactions: Default::default(),
            spents: Default::default(),
        }
    }

    /// check if tx is double spend from the viewpoint of the mempool
    /// doesn't check if chain double spend
    fn is_double_spend(&self, tx: &Transaction) -> bool {
        tx.input
            .iter()
            .any(|input| self.spents.contains_key(&input.previous_output))
    }

    /// add a transaction
    pub fn add_tx(&mut self, chain: &Chain, tx: Transaction) -> Result<(), MempoolError> {
        let lock_flags = LockFlags::STANDARD_LOCKTIME_FLAGS;
        let height = chain.height;
        let hash = tx.txid();

        tx.check_sanity()?;

        if tx.is_coin_base() {
            return Err(MempoolError::Coinbase);
        }

        if !chain.state.has_csv() && tx.version >= 2 {
            return Err(MempoolError::PrematureTxVersion);
        }

        if !chain.state.has_witness() {
            if tx.has_witness() {
                return Err(MempoolError::PrematureWitness);
            }
        }

        if !chain.verify_final(&chain.tip, &tx, &lock_flags) {
            return Err(TransactionVerificationError::NonFinal)?;
        }

        if self.transactions.contains_key(&hash) {
            return Err(MempoolError::Duplicate);
        }

        if chain.db.has_coins(&tx) {
            return Err(MempoolError::Duplicate);
        }

        if self.is_double_spend(&tx) {
            return Err(MempoolError::DoubleSpend);
        }

        let view = self.get_coin_view(chain, &tx);

        if tx
            .input
            .iter()
            .any(|input| !view.map.contains_key(&input.previous_output))
        {
            return Err(MempoolError::Orphan);
        }

        let entry = MemPoolEntry::new(tx, &view, height);

        self.verify(chain, &entry, &view)?;

        for input in &entry.tx.input {
            self.spents.insert(input.previous_output, hash);
        }

        self.transactions.insert(hash, entry);

        debug!(
            "Added {} to mempool (txs={}, spents={}).",
            hash,
            self.transactions.len(),
            self.spents.len()
        );

        Ok(())
    }

    fn verify(
        &self,
        chain: &Chain,
        entry: &MemPoolEntry,
        view: &CoinView,
    ) -> Result<(), MempoolError> {
        let height = chain.height + 1;
        let lock_flags = LockFlags::STANDARD_LOCKTIME_FLAGS;
        let tx = &entry.tx;

        chain.verify_locks(&chain.tip, tx, view, &lock_flags)?;

        tx.check_inputs(view, height)?;

        let flags = ScriptFlags::STANDARD_VERIFY_FLAGS;
        tx.verify_scripts(view, &flags)?;

        Ok(())
    }

    fn get_coin_view(&self, chain: &Chain, tx: &Transaction) -> CoinView {
        let mut view = CoinView::default();

        for input in &tx.input {
            let entry = self.transactions.get(&input.previous_output.txid);

            match entry {
                Some(entry) => {
                    let tx = &entry.tx;
                    if self.has_coin(&input.previous_output) {
                        view.map.insert(
                            input.previous_output,
                            CoinEntry {
                                version: tx.version,
                                coinbase: tx.is_coin_base(),
                                height: None,
                                output: tx.output[input.previous_output.vout as usize].clone(),
                                spent: false,
                            },
                        );
                    }
                }
                None => {
                    if let Some(coin) = chain.db.read_coin(input.previous_output).unwrap() {
                        view.map.insert(input.previous_output, coin);
                    }
                }
            }
        }

        view
    }

    pub fn has_coin(&self, outpoint: &OutPoint) -> bool {
        let entry = match self.transactions.get(&outpoint.txid) {
            None => return false,
            Some(entry) => entry,
        };

        if self.spents.contains_key(outpoint) {
            return false;
        }

        if outpoint.vout as usize >= entry.tx.output.len() {
            return false;
        }

        true
    }

    pub fn remove_block(&mut self, chain: &Chain, entry: &ChainEntry, txs: &[Transaction]) {
        if self.transactions.is_empty() {
            return;
        }

        let mut total = 0;

        for tx in txs.iter().skip(1) {
            let txid = tx.txid();

            if self.transactions.contains_key(&txid) {
                continue;
            }

            if self.add_tx(chain, tx.clone()).is_ok() {
                total += 1;
            }
        }

        if total == 0 {
            return;
        }

        info!(
            "Added {} txs back into the mempool for block {}.",
            total, entry.height
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
    fn remove_spenders(&mut self, entry: &MemPoolEntry) {
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

    fn get_spent(&self, out_point: &OutPoint) -> Option<&MemPoolEntry> {
        self.spents
            .get(out_point)
            .map(|txid| &self.transactions[txid])
    }

    fn remove_spent(&mut self, out_point: &OutPoint) -> Option<MemPoolEntry> {
        self.spents
            .remove(out_point)
            .map(|txid| self.transactions.remove(&txid).unwrap())
    }
}
