use super::{ChainDB, ChainEntry};
use crate::coins::CoinView;
use crate::net::PeerId;
use crate::protocol::NetworkParams;
use crate::util::EmptyResult;
use bitcoin::{hashes::sha256d::Hash as H256, Block};
use failure::{ensure, Error};
use std::sync::Arc;

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
    fn handle_bad_orphan(&self, _error: &Error, _peer_id: &PeerId) {}
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

#[derive(Default)]
pub struct Chain {
    db: ChainDB,
    pub tip: ChainEntry,
    height: u32,
    listeners: Vec<Arc<dyn ChainListener>>,
    options: ChainOptions,
}

impl Chain {
    pub fn new(network_params: NetworkParams) -> Self {
        Self {
            db: ChainDB::new(network_params.clone()),
            tip: ChainEntry::default(),
            height: 0,
            listeners: vec![],
            options: Default::default(),
        }
    }

    pub fn open(&mut self) -> EmptyResult {
        self.db.open()?;
        self.tip = self.db.get_tip()?.unwrap().clone();
        Ok(())
    }

    /// Add a block to the chain and return a chain entry representing it if it was added successfully
    pub fn add(&mut self, block: &Block) -> Result<Option<ChainEntry>, Error> {
        block.header.validate_pow(&block.header.target())?;
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
        self.verify(&block, prev)?;

        if self.is_historical(prev) {
            return self.update_inputs(block, prev);
        } else {
            todo!("full verification");
        }
    }

    fn verify(&self, block: &Block, prev: &ChainEntry) -> EmptyResult {
        ensure!(block.header.prev_blockhash == prev.hash);

        // TODO: verify checkpoint

        if self.is_historical(prev) {
            ensure!(block.check_merkle_root());
            Ok(())
        } else {
            todo!("full verification");
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
        prev.height + 1 <= self.options.network.last_checkpoint
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
}

#[derive(Default)]
struct ChainOptions {
    network: NetworkParams,
}
