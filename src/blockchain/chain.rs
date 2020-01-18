use super::{ChainDB, ChainDBOptions, ChainEntry};
use crate::coins::CoinView;
use crate::net::PeerId;
use crate::protocol::{consensus, NetworkParams};
use crate::util::EmptyResult;
use crate::verification::{BlockVerifier, TransactionVerifier};
use crate::verification::{CheckQueue, CheckQueueControl};
use bitcoin::{
    blockdata::constants::{DIFFCHANGE_INTERVAL, DIFFCHANGE_TIMESPAN, MAX_BLOCK_WEIGHT},
    hashes::sha256d::Hash as H256,
    util::uint::Uint256,
    Block, BlockHeader,
};
use failure::{bail, ensure, err_msg, Error};
use std::{path::PathBuf, sync::Arc};

/// Trait that handles chain events
pub trait ChainListener: Send + Sync + 'static {
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
    verifier: CheckQueueControl,
}

impl Chain {
    pub fn new(options: ChainOptions) -> Self {
        let verifier_queue = CheckQueue::new(128);
        for _ in 0..8 {
            verifier_queue.thread();
        }
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
            verifier: CheckQueueControl::new(verifier_queue),
        }
    }

    pub fn open(&mut self) -> EmptyResult {
        self.db.open()?;
        self.tip = *self.db.get_tip()?.unwrap();
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
            return self.update_inputs(block, prev);
        }

        // TODO: verify duplicate txids

        // do full verification
        let view = self.verify_inputs(block, prev)?;

        Ok(view)
    }

    fn verify_inputs(&mut self, block: &Block, prev: &ChainEntry) -> Result<CoinView, Error> {
        use bitcoin::util::amount::Amount;

        let mut view = CoinView::default();
        let height = prev.height + 1;

        let mut sigops = 0;
        let mut reward = Amount::ZERO;

        for (i, tx) in block.txdata.iter().enumerate() {
            if i > 0 {
                view.spend_inputs(&mut self.db, tx)?;
            }

            if i > 0 && tx.version >= 2 {
                // todo!("verify locktime");
            }

            sigops += tx.get_sigop_cost(&view);

            if sigops > consensus::MAX_BLOCK_SIGOPS_COST {
                bail!("bad-blk-sigops");
            }

            if i > 0 {
                let fee = tx.check_inputs(&view, height)?;

                reward = reward
                    .checked_add(fee)
                    .ok_or(err_msg("bad-txns-inputvalues-outofrange"))?;

                if reward.as_sat() > consensus::MAX_MONEY {
                    bail!("bad-txns-accumulated-fee-outofrange");
                }
            }

            view.add_tx(tx, height);
        }

        reward += Amount::from_sat(consensus::get_block_subsidy(height, &self.options.network));

        if block.get_claimed()? > reward.as_sat() {
            bail!("bad-cb-amount");
        }

        let mut flags = bitcoinconsensus::VERIFY_NONE;

        // p2sh height is wrong in bitcoinconsensus
        if height > 173804 {
            flags = bitcoinconsensus::height_to_flags(height);
        }

        // Script verification for all but the coinbase
        for tx in block.txdata.iter().skip(1) {
            tx.push_verification(&view, flags, &self.verifier);
        }

        // tell tokio that this thread will be blocked
        ensure!(tokio::task::block_in_place(|| self.verifier.wait()));

        Ok(view)
    }

    // fn get_median_time(&self, prev: &ChainEntry, time: Option<u32>) -> usize {
    //     todo!()
    // }

    fn verify(&mut self, block: &Block, prev: &ChainEntry) -> EmptyResult {
        ensure!(block.header.prev_blockhash == prev.hash);

        let height = prev.height + 1;

        // TODO: verify checkpoint

        if self.is_historical(prev) {
            // TODO: Check for merkle tree malleability
            ensure!(block.check_merkle_root());
            Ok(())
        } else {
            // non contextual block verification
            block.check_body()?;

            // check block bits are correct and retarget if necessary
            let bits = self.get_target(block.header.time, Some(prev))?;
            ensure!(
                bits == block.header.bits,
                "bad-diffbits {} vs {}",
                bits,
                block.header.bits
            );

            // TODO: check time

            // TODO: check BIPs

            // TODO: check tx sequences and locks

            // coinbase height must be the first part of a coinbase scriptsig once bip34 is activated
            if height >= 227_931 {
                block.check_coinbase_height(prev.height + 1)?;
            }

            // Check witness commitment hash.
            // Returns true if block is not segwit so always check
            ensure!(block.check_witness_commitment());

            // check block weight
            ensure!(block.get_weight() <= MAX_BLOCK_WEIGHT as usize);

            Ok(())
        }
    }

    fn get_target(&mut self, _time: u32, prev: Option<&ChainEntry>) -> Result<u32, Error> {
        let prev = match prev {
            Some(prev) => prev,
            None => {
                return Ok(self.options.network.pow_limit_bits);
            }
        };

        // don't retarget
        if (prev.height + 1) % DIFFCHANGE_INTERVAL != 0 {
            // TODO: reset target for testnet if required
            return Ok(prev.bits);
        }

        ensure!(prev.height >= DIFFCHANGE_INTERVAL - 1);

        let height = prev.height - (DIFFCHANGE_INTERVAL - 1);

        let first = self
            .db
            .get_ancestor(prev, height)?
            .ok_or(err_msg("missing-ancestor"))?;

        let new_target_bits = self.retarget(prev, &first)?;

        Ok(new_target_bits)
    }

    fn retarget(&self, prev: &ChainEntry, first: &ChainEntry) -> Result<u32, Error> {
        // don't retarget on regtest
        if self.options.network.no_pow_retargeting {
            return Ok(prev.bits);
        }

        let pow_limit = &self.options.network.pow_limit;
        let pow_limit_bits = self.options.network.pow_limit_bits;

        let mut target = consensus::compact_to_target(prev.bits);

        let mut actual_timespan = prev.time - first.time;

        // timespan can only be increased or decreased by 25%
        if actual_timespan < DIFFCHANGE_TIMESPAN / 4 {
            actual_timespan = DIFFCHANGE_TIMESPAN / 4;
        }

        if actual_timespan > DIFFCHANGE_TIMESPAN * 4 {
            actual_timespan = DIFFCHANGE_TIMESPAN * 4;
        }

        target = target.mul_u32(actual_timespan);
        target = target / Uint256::from_u64(DIFFCHANGE_TIMESPAN as u64).unwrap();

        if &target > pow_limit {
            return Ok(pow_limit_bits);
        }

        Ok(BlockHeader::compact_target_from_u256(&target))
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

    fn is_historical(&self, _prev: &ChainEntry) -> bool {
        // prev.height < self.options.network.last_checkpoint
        false
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
