use super::{ChainDB, ChainDBOptions, ChainEntry};
use crate::coins::CoinView;
use crate::net::PeerId;
use crate::protocol::{consensus, NetworkParams};
use crate::util::EmptyResult;
use crate::verification::{BlockVerifier, TransactionVerifier};
use bitcoin::{hashes::sha256d::Hash as H256, Block};
use failure::{bail, ensure, Error};
use std::{path::PathBuf, sync::Arc};

/// Trait that handles chain events
pub trait ChainListener {
    /// Called when a new block is added to the main chain
    fn handle_connect(&self, _entry: &ChainEntry, _block: &Block, _view: &CoinView) {}
    /// Called when a block is removed from the main chain. This happens
    /// during a reorg or manual reindex
    fn handle_disconnect(&self, _entry: &ChainEntry, _block: &Block, _view: &CoinView) {}
    /// Called when a block cannot be added to the main chain because the
    /// previous block is not the tip of the main chain
    fn handle_orphan(&self, _block: &Block) {}
    /// Called when a valid block is recieved
    fn handle_block(&self, _block: &Block, _entry: &ChainEntry) {}
    /// Called when the chain is reset to a certain block
    fn handle_reset(&self, _tip: &ChainEntry) {}
    /// Called when the chain is fully synced
    fn handle_full(&self) {}
    /// Called when an orphan block couldn't be connected
    fn handle_bad_orphan(&self, _error: &Error, _peer_id: PeerId) {}
    /// Called when the blockchain tip changes
    fn handle_tip(&self, _tip: &ChainEntry) {}
    /// Called when a competing chain with higher chainwork is received
    fn handle_reorg(&self, _tip: &ChainEntry, _competitor: &ChainEntry) {}
    /// Called when blocks from a competing chain are connected to the main chain
    fn handle_reconnect(&self, _entry: &ChainEntry, _block: &Block) {}
    /// Called when a block from an alternate chain is received.
    /// An alternate chain is one with the same of less chainwork than the main chain that both have
    fn handle_competitor(&self, _block: &Block, _entry: &ChainEntry) {}

    fn handle_resolved(&self, _block: &Block, _entry: &ChainEntry) {}
    fn handle_checkpoint(&self, _hash: H256, _height: u32) {}
}

pub struct Chain {
    pub db: ChainDB,
    pub tip: ChainEntry,
    pub height: u32,
    listeners: Vec<Arc<dyn ChainListener>>,
    options: ChainOptions,
}

impl Chain {
    pub fn new(options: ChainOptions) -> Self {
        Self {
            db: ChainDB::new(
                options.network.clone(),
                ChainDBOptions {
                    path: options.path.clone(),
                },
            ),
            tip: ChainEntry::default(),
            height: 0,
            listeners: vec![],
            options,
        }
    }

    pub fn open(&mut self) -> EmptyResult {
        self.db.open()?;
        self.tip = self.db.get_tip()?.unwrap().clone();
        Ok(())
    }

    /// Add a block to the chain and return a chain entry representing it if it was added successfully
    pub fn add(&mut self, block: &Block) -> Result<Option<ChainEntry>, Error> {
        block.validate_pow()?;
        let prev = match self.db.get_entry_by_hash(block.header.prev_blockhash)? {
            None => todo!("orphan block"),
            Some(prev) => prev.clone(),
        };
        let entry = self.connect(&prev, block)?;
        Ok(Some(entry))
    }

    fn connect(&mut self, prev: &ChainEntry, block: &Block) -> Result<ChainEntry, Error> {
        ensure!(block.header.prev_blockhash == prev.hash);

        let entry = ChainEntry::from_block(block, Some(prev));

        if entry.chainwork <= self.tip.chainwork {
            todo!("forked chain");
        } else {
            self.set_best_chain(&entry, block, prev)?;
        }
        Ok(entry)
    }

    fn set_best_chain(
        &mut self,
        entry: &ChainEntry,
        block: &Block,
        prev: &ChainEntry,
    ) -> EmptyResult {
        if entry.prev_block != self.tip.hash {
            todo!("reorg");
        }

        let view = self.verify_context(block, prev)?;

        self.db.save(entry, block, Some(&view))?;

        self.tip = entry.clone();
        self.height = entry.height;

        self.notify_tip(entry);
        self.notify_block(block, entry);
        self.notify_connect(entry, block, &view);

        Ok(())
    }

    fn verify_context(&mut self, block: &Block, prev: &ChainEntry) -> Result<CoinView, Error> {
        // non contextual verification;
        self.verify(&block, prev)?;

        if self.is_historical(prev) {
            // skip full verification if checkpoint block
            self.update_inputs(block, prev)
        } else {
            // TODO: verify duplicate txids

            // do full verification
            self.verify_inputs(block, prev)
        }
    }

    fn verify_inputs(&mut self, block: &Block, prev: &ChainEntry) -> Result<CoinView, Error> {
        let mut view = CoinView::default();
        let height = prev.height + 1;

        // let mut sigops = 0;
        let mut reward = 0;

        for (i, tx) in block.txdata.iter().enumerate() {
            if i > 0 {
                view.spend_inputs(&mut self.db, tx)?;
            }

            if i > 0 && tx.version >= 2 {
                todo!("verify locktime");
            }

            // TODO: sigops
            // sigops += tx.get_sigops_cost(&view);

            // if sigops > consensus::MAX_BLOCK_SIGOPS_COST {
            //     bail!("bad-blk-sigops");
            // }

            if i > 0 {
                let fee = tx.check_inputs(&view, height)?;
                reward += fee;

                if reward > consensus::MAX_MONEY {
                    bail!("bad-txns-accumulated-fee-outofrange");
                }
            }

            view.add_tx(tx, height);
        }

        reward += consensus::get_block_subsidy(height, &self.options.network);

        if block.get_claimed()? > reward {
            bail!("bad-cb-amount");
        }

        // TODO: script verification

        Ok(view)
    }

    fn verify(&self, block: &Block, prev: &ChainEntry) -> EmptyResult {
        ensure!(block.header.prev_blockhash == prev.hash);

        // TODO: verify checkpoint

        if self.is_historical(prev) {
            ensure!(block.check_merkle_root());
            Ok(())
        } else {
            // TODO: non contextual block verification

            // TODO: check block bits correct

            // TODO: check time

            // TODO: check BIPs

            // TODO: check tx sequences and locks

            // TODO: check block weight

            Ok(())
        }
    }

    fn update_inputs(&mut self, block: &Block, prev: &ChainEntry) -> Result<CoinView, Error> {
        let mut view = CoinView::default();
        let height = prev.height + 1;
        let coinbase = &block.txdata[0];

        view.add_tx(coinbase, height);

        for tx in block.txdata.iter().skip(1) {
            view.spend_inputs(&mut self.db, tx)?;
            view.add_tx(tx, height);
        }

        Ok(view)
    }

    fn is_historical(&self, prev: &ChainEntry) -> bool {
        prev.height < self.options.network.last_checkpoint
    }

    fn notify_block(&self, block: &Block, entry: &ChainEntry) {
        for listener in &self.listeners {
            listener.handle_block(block, entry);
        }
    }

    fn notify_tip(&self, tip: &ChainEntry) {
        for listener in &self.listeners {
            listener.handle_tip(tip);
        }
    }

    fn notify_connect(&self, entry: &ChainEntry, block: &Block, view: &CoinView) {
        for listener in &self.listeners {
            listener.handle_connect(entry, block, view);
        }
    }

    pub fn add_listener(&mut self, listener: Arc<dyn ChainListener>) {
        self.listeners.push(listener);
    }
}

pub struct ChainOptions {
    pub network: NetworkParams,
    pub path: PathBuf,
}
