use super::{ChainDB, ChainDBOptions, ChainEntry};
use crate::coins::CoinView;
use crate::net::PeerId;
use crate::protocol::{
    consensus::{self, LockFlags, ScriptFlags},
    BIP9Deployment, NetworkParams, ThresholdState,
};
use crate::util::EmptyResult;
use crate::verification::{BlockVerifier, TransactionVerifier};
use crate::verification::{CheckQueue, CheckQueueControl};
use bitcoin::{
    blockdata::constants::{DIFFCHANGE_INTERVAL, DIFFCHANGE_TIMESPAN, MAX_BLOCK_WEIGHT},
    hashes::sha256d::Hash as H256,
    util::uint::Uint256,
    Block, BlockHash, BlockHeader, Transaction,
};
use chrono::{TimeZone, Utc};
use failure::{bail, ensure, err_msg, Error};
use futures::future::{BoxFuture, FutureExt};
use log::{debug, info, warn};
use std::collections::VecDeque;
use std::{path::PathBuf, sync::Arc, time::SystemTime};

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
    state: DeploymentState,
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
            state: DeploymentState::default(),
        }
    }

    pub async fn open(&mut self) -> EmptyResult {
        self.db.open().await?;

        let tip = *self
            .db
            .get_tip()?
            .expect("always tip if chain db opened first");

        self.height = tip.height;
        self.tip = tip;

        info!("Chain Height: {}", self.tip.height);

        let state = self.get_deployment_state()?;
        self.set_deployment_state(state);

        Ok(())
    }

    fn set_deployment_state(&mut self, state: DeploymentState) {
        if !self.state.has_p2sh() && state.has_p2sh() {
            info!("P2SH has been activated");
        }

        if !self.state.has_bip34() && state.has_bip34() {
            info!("BIP34 has been activated");
        }

        if !self.state.has_bip66() && state.has_bip66() {
            info!("BIP66 has been activated");
        }

        if !self.state.has_cltv() && state.has_cltv() {
            info!("CLTV has been activated");
        }

        if !self.state.has_csv() && state.has_csv() {
            info!("CSV has been activated");
        }

        if !self.state.has_witness() && state.has_witness() {
            info!("Segwit has been activated");
        }

        if !self.state.has_taproot() && state.has_taproot() {
            info!("Taproot has been activated");
        }

        if !self.state.has_ctv() && state.has_ctv() {
            info!("CTV has been activated");
        }

        self.state = state;
    }

    async fn is_block_ready(&self, hash: BlockHash) -> Result<bool, Error> {
        let has = !self.db.has_invalid(&hash) && self.db.has_block(hash).await?;
        Ok(has)
    }

    /// Add a block to the chain and return a chain entry representing it if it was added successfully
    pub async fn add(&mut self, block: Block) -> Result<Option<ChainEntry>, Error> {
        let (entry, prev) = match self.db.get_entry_by_hash(block.block_hash())? {
            Some(entry) => {
                let entry = *entry;
                let prev = *self
                    .db
                    .get_entry_by_hash(block.header.prev_blockhash)?
                    .expect("prev block to be added before this");
                (entry, prev)
            }
            None => self.add_header(&block.header)?,
        };

        self.add_block(&block, Some(entry), Some(prev)).await?;
        self.connect_best(entry, prev, Some(block)).await?;

        Ok(Some(entry))
    }

    async fn add_block(
        &mut self,
        block: &Block,
        entry: Option<ChainEntry>,
        prev: Option<ChainEntry>,
    ) -> EmptyResult {
        let hash = block.block_hash();

        let entry = if let Some(entry) = entry {
            entry
        } else {
            *self
                .db
                .get_entry_by_hash(hash)?
                .expect("block only added if we have header")
        };

        let prev = if let Some(prev) = prev {
            prev
        } else {
            *self
                .db
                .get_entry_by_hash(block.header.prev_blockhash)?
                .expect("block only added if we have previous header")
        };

        if self.db.has_block(hash).await? {
            bail!("duplicate block {}", hash);
        }

        if let Err(err) = self.verify(block, prev) {
            // check if mutated, don't add to invalid set if mutated
            self.db.set_invalid(entry.hash);
            return Err(err);
        }

        self.db.write_block(block).await?;

        if entry.chainwork <= self.tip.chainwork {
            warn!("Heads up: Competing chain at height {}: tip-height={} competitor-height={} tip-hash={} competitor-hash={} tip-chainwork={} competitor-chainwork={} chainwork-diff={}", entry.height, self.tip.height, entry.height, self.tip.hash, entry.hash, self.tip.chainwork, entry.chainwork, self.tip.chainwork - entry.chainwork);
        }

        Ok(())
    }

    pub fn find_locator(&mut self, locator: Vec<BlockHash>) -> Result<BlockHash, Error> {
        for hash in locator {
            if self.db.is_main_hash(hash)? {
                return Ok(hash);
            }
        }

        Ok(bitcoin::blockdata::constants::genesis_block(self.options.network.network).block_hash())
    }

    // should be one for one match with `AcceptBlockHeader` in core
    pub fn add_header(&mut self, header: &BlockHeader) -> Result<(ChainEntry, ChainEntry), Error> {
        header.validate_pow(&header.target())?;

        let hash = header.block_hash();

        ensure!(!self.has_invalid(hash, header.prev_blockhash));

        ensure!(!self.db.has_header(hash)?);

        let prev = *self
            .db
            .get_entry_by_hash(header.prev_blockhash)?
            .ok_or_else(|| err_msg("bad-prevblk"))?;

        let entry = ChainEntry::from_block_header(header, Some(&prev));

        if let Err(err) = self.contextual_check_block_header(header, prev) {
            // TODO: check for mutation and set invalid if not mutated
            return Err(err);
        };

        self.db.save_entry(entry, &prev)?;

        self.log_headers(header, &entry);

        Ok((entry, prev))
    }

    fn log_headers(&self, header: &BlockHeader, entry: &ChainEntry) {
        if entry.height % 6000 == 0 {
            info!(
                "Headers Status: hash = {} time = {} height = {} target = {}",
                entry.hash,
                Utc.timestamp(header.time as i64, 0).format("%d-%m-%Y %r"),
                entry.height,
                header.bits
            );
        }
    }

    pub async fn attach(&mut self, entry: ChainEntry) -> EmptyResult {
        if self.db.is_main_chain(&entry)? {
            return Ok(());
        }

        let prev = *self
            .db
            .get_entry_by_hash(entry.prev_block)?
            .expect("prev already checked to exist before attach");

        self.connect_best(entry, prev, None).await?;

        Ok(())
    }

    fn _get_next_best(
        &mut self,
        entry: ChainEntry,
    ) -> BoxFuture<Result<VecDeque<ChainEntry>, Error>> {
        async move {
            let mut max_work = Uint256::from_u64(0).unwrap();
            let mut max_path = VecDeque::new();

            if !self.is_block_ready(entry.hash).await? {
                return Ok(max_path);
            }

            let entries = self.db.get_next_entries(entry.hash)?;

            for next in entries {
                let path = self._get_next_best(next).await?;
                if let Some(last) = path.back() {
                    let chainwork = last.chainwork;
                    if chainwork > max_work {
                        max_work = chainwork;
                        max_path = path;
                    }
                }
            }

            max_path.push_front(entry);

            Ok(max_path)
        }
        .boxed()
    }

    async fn get_next_best(&mut self, entry: ChainEntry) -> Result<VecDeque<ChainEntry>, Error> {
        let mut path = self._get_next_best(entry).await?;
        if !path.is_empty() {
            path.pop_front();
        }
        Ok(path)
    }

    async fn connect_best(
        &mut self,
        mut entry: ChainEntry,
        mut prev: ChainEntry,
        block: Option<Block>,
    ) -> EmptyResult {
        // let tip = self.tip;

        assert!(entry.prev_block == prev.hash);

        let mut queue = VecDeque::new();

        if self.is_block_ready(entry.hash).await? {
            queue.push_back((entry, prev, block));

            let mut head;
            let mut head_prev = prev;

            while !self.db.is_main_chain(&head_prev)? {
                if self.is_block_ready(head_prev.hash).await? {
                    head = head_prev;
                    head_prev = *self.db.get_entry_by_hash(head.prev_block)?.unwrap();
                    queue.push_front((head, head_prev, None));
                } else {
                    return Ok(());
                }
            }

            // See if there are any blocks following
            // that are waiting to be connected.
            let nexts = self.get_next_best(entry).await?;
            for next in nexts {
                prev = entry;
                entry = next;
                queue.push_back((entry, prev, None));
            }

            // Verify that this new set of entries has greater
            // work than the current chain.

            let (last_entry, _, _) = queue.back().expect("always push to queue first");
            if last_entry.chainwork <= self.tip.chainwork {
                queue.clear();
            }
        }

        if queue.is_empty() {
            return Ok(());
        }

        // let common = self.tip;

        for (entry, prev, block) in queue {
            let block = if let Some(block) = block {
                block
            } else {
                self.db.get_block(entry.hash).await?.unwrap()
            };

            self.connect(&block, entry, prev)?;
        }

        Ok(())
    }

    // should be one for one match with core `ContextualCheckBlockHeader`
    fn contextual_check_block_header(
        &mut self,
        header: &BlockHeader,
        prev: ChainEntry,
    ) -> EmptyResult {
        ensure!(header.prev_blockhash == prev.hash, "bad-prevblk");

        // check block bits are correct and retarget if necessary
        let bits = self.get_target(header.time, Some(&prev))?;
        ensure!(bits == header.bits, "bad-diffbits");

        let mtp = self.get_median_time(prev)?;
        ensure!(header.time > mtp, "time-too-old");

        let adjusted_time = self
            .options
            .network
            .time
            .lock()
            .unwrap()
            .get_adjusted_time();

        ensure!(
            (header.time as u64) < adjusted_time + consensus::MAX_FUTURE_BLOCK_TIME as u64,
            "time-too-new"
        );

        let height = prev.height;

        // TODO: Check against checkpoints
        // Don't accept any forks from the main chain prior to last checkpoint.

        // Reject outdated version blocks when 95% (75% on testnet) of the network has upgraded:
        // check for version 2, 3 and 4 upgrades
        if header.version < 2 && height >= self.options.network.bip34_height
            || header.version < 3 && height >= self.options.network.bip66_height
            || header.version < 4 && height >= self.options.network.bip65_height
        {
            bail!("bad-version")
        }

        Ok(())
    }

    pub fn common_ancestor(
        &mut self,
        a: ChainEntry,
        b: ChainEntry,
    ) -> Result<Option<ChainEntry>, Error> {
        use std::cmp::Ordering;

        let (mut a, mut b) = {
            match a.height.cmp(&b.height) {
                Ordering::Equal => (Some(a), Some(b)),
                Ordering::Greater => {
                    let a = self.db.get_ancestor(&a, b.height)?;
                    (Some(a), Some(b))
                }
                Ordering::Less => {
                    let b = self.db.get_ancestor(&b, a.height)?;
                    (Some(a), Some(b))
                }
            }
        };

        while let Some((aa, bb)) = match (a, b) {
            (Some(a), Some(b)) if a.hash != b.hash => Some((a, b)),
            _ => None,
        } {
            a = self.db.get_entry_by_hash(aa.prev_block)?.copied();
            b = self.db.get_entry_by_hash(bb.prev_block)?.copied();
        }

        if a.is_some() && b.is_some() {
            Ok(a)
        } else {
            Ok(None)
        }
    }

    fn has_invalid(&mut self, hash: BlockHash, prev: BlockHash) -> bool {
        if self.db.has_invalid(&hash) {
            return true;
        }

        if self.db.has_invalid(&prev) {
            self.db.set_invalid(hash);
            return true;
        }

        false
    }

    pub fn get_locator(&mut self, start: Option<BlockHash>) -> Result<Vec<BlockHash>, Error> {
        let start = start.unwrap_or(self.tip.hash);

        let entry = self.db.get_entry_by_hash(start)?;

        let mut hashes = vec![];

        let entry = match entry {
            Some(entry) => *entry,
            None => {
                hashes.push(start);
                self.tip
            }
        };

        let mut main = self.db.is_main_chain(&entry)?;
        let mut hash = entry.hash;
        let mut height = entry.height as i32;

        let mut step = 1;

        hashes.push(hash);

        while height > 0 {
            height -= step;

            if height < 0 {
                height = 0;
            }

            if hashes.len() > 10 {
                step *= 2;
            }

            if main {
                hash = self
                    .db
                    .get_entry_by_height(height as u32)?
                    .expect("main chain so must exist by height")
                    .hash;
            } else {
                let ancestor = self.db.get_ancestor(&entry, height as u32)?;
                main = self.db.is_main_chain(&ancestor)?;
                hash = ancestor.hash;
            }

            hashes.push(hash);
        }

        Ok(hashes)
    }

    pub fn is_recent(&self) -> bool {
        let best = self.most_work();
        let time = crate::util::now() - self.options.network.max_tip_age as u64;

        best.time > time as u32
    }

    pub fn most_work(&self) -> ChainEntry {
        self.db.most_work.expect("chain db to be open")
    }

    fn connect(
        &mut self,
        block: &Block,
        entry: ChainEntry,
        prev: ChainEntry,
    ) -> Result<ChainEntry, Error> {
        ensure!(block.header.prev_blockhash == prev.hash);
        ensure!(prev.hash == self.tip.hash);

        let start = SystemTime::now();

        let (view, state) = self.verify_context(block, prev)?;

        self.db.connect(entry, block, view)?;

        self.height = entry.height;
        self.tip = entry;

        self.set_deployment_state(state);

        if entry.height % 20 == 0 || entry.height > 350_000 {
            debug!(
                "Block {} ({}) added to chain (size={} txs={} time={}ms)",
                entry.hash,
                entry.height,
                block.get_size(),
                block.txdata.len(),
                ms_since(&start)
            );
        }

        Ok(entry)
    }

    fn verify_context(
        &mut self,
        block: &Block,
        prev: ChainEntry,
    ) -> Result<(CoinView, DeploymentState), Error> {
        let state = self.get_deployments(block.header.time, prev)?;

        // non contextual verification;
        self.verify(&block, prev)?;

        if self.is_historical(&prev) {
            // skip full verification if checkpoint block
            let view = self.update_inputs(block, &prev)?;
            return Ok((view, state));
        }

        // verify duplicate txids
        if !state.has_bip34() || prev.height + 1 >= consensus::BIP34_IMPLIES_BIP30_LIMIT {
            ensure!(self.verify_duplicates(block, &prev), "bad-txns-BIP30");
        }

        // do full verification
        let view = self.verify_inputs(block, &prev, &state)?;

        Ok((view, state))
    }

    fn verify_duplicates(&mut self, block: &Block, prev: &ChainEntry) -> bool {
        for tx in &block.txdata {
            if !self.db.has_coins(tx) {
                continue;
            }

            let height = prev.height + 1;
            let hash = self.options.network.bip30.get(&height);

            match hash {
                Some(hash) if hash != &block.block_hash() => return false,
                None => return false,
                _ => (),
            }
        }
        true
    }

    fn verify_inputs(
        &mut self,
        block: &Block,
        prev: &ChainEntry,
        state: &DeploymentState,
    ) -> Result<CoinView, Error> {
        let mut view = CoinView::default();
        let height = prev.height + 1;

        let mut sigops = 0;
        let mut reward: u64 = 0;

        for (i, tx) in block.txdata.iter().enumerate() {
            if i > 0 {
                view.spend_inputs(&mut self.db, tx)?;
            }

            if i > 0 && tx.version >= 2 {
                ensure!(
                    self.verify_locks(prev, tx, &view, &state.lock_flags),
                    "bad-txns-nonfinal"
                );
            }

            sigops += tx.get_sigop_cost(&view, state.script_flags);

            ensure!(sigops <= consensus::MAX_BLOCK_SIGOPS_COST, "bad-blk-sigops");

            if i > 0 {
                let fee = tx.check_inputs(&view, height)?;

                reward = reward
                    .checked_add(fee)
                    .ok_or_else(|| err_msg("bad-txns-inputvalues-outofrange"))?;

                ensure!(
                    reward <= consensus::MAX_MONEY,
                    "bad-txns-accumulated-fee-outofrange"
                );
            }

            view.add_tx(tx, height);
        }

        reward += consensus::get_block_subsidy(height, &self.options.network);

        ensure!(block.get_claimed()? <= reward, "bad-cb-amount");

        // Script verification for all but the coinbase
        for tx in block.txdata.iter().skip(1) {
            tx.push_verification(&view, state.script_flags, &self.verifier);
        }

        // tell tokio that this thread will be blocked
        ensure!(tokio::task::block_in_place(|| self.verifier.wait()));

        Ok(view)
    }

    fn get_median_time(&mut self, prev: ChainEntry) -> Result<u32, Error> {
        let mut median = Vec::with_capacity(consensus::MEDIAN_TIMESPAN);
        let mut entry = Some(prev);

        for _ in 0..consensus::MEDIAN_TIMESPAN {
            if let Some(chain_entry) = entry {
                median.push(chain_entry.time);
                entry = self.db.get_entry_by_hash(chain_entry.prev_block)?.copied();
            } else {
                break;
            }
        }

        median.sort_unstable();

        Ok(median[median.len() / 2])
    }

    fn verify(&mut self, block: &Block, prev: ChainEntry) -> EmptyResult {
        ensure!(block.header.prev_blockhash == prev.hash, "bad-prevblk");

        // TODO: verify checkpoint

        if self.is_historical(&prev) {
            // TODO: Check for merkle tree malleability
            ensure!(block.check_merkle_root(), "bad-txnmrklroot");
            Ok(())
        } else {
            // non contextual block verification
            block.check_body()?;

            let height = prev.height + 1;

            let state = self.get_deployments(block.header.time, prev)?;

            let mtp = self.get_median_time(prev)?;
            let time = if state.has_mtp() {
                mtp
            } else {
                block.header.time
            };

            // Transactions must be finalized with
            // regards to nSequence and nLockTime.
            for tx in &block.txdata {
                ensure!(tx.is_final(height, time), "bad-txns-nonfinal");
            }

            // bip34 made coinbase txs unique by including the height
            // of the block in the scriptsig
            if state.has_bip34() {
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

    fn get_target(&mut self, time: u32, prev: Option<&ChainEntry>) -> Result<u32, Error> {
        let pow_limit_bits = self.options.network.pow_limit_bits;

        let prev = match prev {
            Some(prev) => prev,
            None => {
                return Ok(pow_limit_bits);
            }
        };

        // Only change once per difficulty adjustment interval
        if (prev.height + 1) % DIFFCHANGE_INTERVAL != 0 {
            let mut prev = *prev;

            // Special difficulty rule for testnet:
            // If the new block's timestamp is more than 2 * 10 minutes
            // then allow mining of a min-difficulty block.
            if self.options.network.allow_min_difficulty_blocks {
                if time > prev.time + self.options.network.pow_target_spacing * 2 {
                    return Ok(pow_limit_bits);
                }
                while prev.height != 0
                    && prev.height % DIFFCHANGE_INTERVAL != 0
                    && prev.bits == pow_limit_bits
                {
                    prev = *self
                        .db
                        .get_entry_by_hash(prev.prev_block)?
                        .ok_or_else(|| err_msg("missing block"))?;
                }
            }
            return Ok(prev.bits);
        }

        ensure!(prev.height >= DIFFCHANGE_INTERVAL - 1);

        // Go back by what we want to be 14 days worth of blocks
        let height = prev.height - (DIFFCHANGE_INTERVAL - 1);

        let first = self.db.get_ancestor(prev, height)?;

        let new_target_bits = self.retarget(prev, &first)?;

        Ok(new_target_bits)
    }

    fn retarget(&self, prev: &ChainEntry, first: &ChainEntry) -> Result<u32, Error> {
        // don't retarget on regtest
        if self.options.network.no_pow_retargeting {
            return Ok(prev.bits);
        }

        let pow_limit = self.options.network.pow_limit;
        let pow_limit_bits = self.options.network.pow_limit_bits;

        let mut target = consensus::compact_to_target(prev.bits);

        let mut actual_timespan = prev.time - first.time;

        // max decrease is 75%
        if actual_timespan < DIFFCHANGE_TIMESPAN / 4 {
            actual_timespan = DIFFCHANGE_TIMESPAN / 4;
        }

        // max increase is 300%
        if actual_timespan > DIFFCHANGE_TIMESPAN * 4 {
            actual_timespan = DIFFCHANGE_TIMESPAN * 4;
        }

        target = target * Uint256::from_u64(actual_timespan as u64).unwrap();
        target = target / Uint256::from_u64(DIFFCHANGE_TIMESPAN as u64).unwrap();

        if target > pow_limit {
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

    fn get_deployment_status(
        &mut self,
        mut prev: ChainEntry,
        deployment: BIP9Deployment,
    ) -> Result<ThresholdState, Error> {
        let bit = deployment.bit;

        if deployment.start_time == BIP9Deployment::ALWAYS_ACTIVE {
            return Ok(ThresholdState::Active);
        }

        // checked was positive above
        let start_time = deployment.start_time as u32;
        let timeout = deployment.timeout;
        let window = self.options.network.miner_confirmation_window;
        let threshold = self.options.network.rule_change_activation_threshold;

        // All blocks within a retarget period have the same state
        if ((prev.height + 1) % window) != 0 {
            let height = prev.height.checked_sub((prev.height + 1) % window);
            if let Some(height) = height {
                prev = self.db.get_ancestor(&prev, height)?;

                assert!(prev.height == height);
                assert!(((prev.height + 1) % window) == 0);
            } else {
                return Ok(ThresholdState::Defined);
            }
        }

        let mut entry = prev;
        let mut state = ThresholdState::Defined;

        let mut compute = vec![];

        loop {
            if let Some(cached) = self.db.version_bits_cache.get(bit, &entry.hash) {
                state = cached;
                break;
            }
            let time = self.get_median_time(entry)?;
            if time < start_time {
                state = ThresholdState::Defined;
                self.db.version_bits_cache.set(bit, entry.hash, state);
                break;
            }
            compute.push(entry);
            let height = entry.height.checked_sub(window);
            if let Some(height) = height {
                entry = self.db.get_ancestor(&entry, height)?;
            } else {
                break;
            }
        }

        for entry in compute {
            match state {
                ThresholdState::Defined => {
                    let time = self.get_median_time(entry)?;
                    if time >= timeout {
                        state = ThresholdState::Failed;
                    } else if time >= start_time {
                        state = ThresholdState::Started;
                    }
                }
                ThresholdState::Started => {
                    let time = self.get_median_time(entry)?;
                    if time >= timeout {
                        state = ThresholdState::Failed;
                    } else {
                        let mut block = entry;
                        let mut count = 0;
                        for _ in 0..window {
                            if block.has_bit(bit) {
                                count += 1;
                            }
                            if count >= threshold {
                                state = ThresholdState::LockedIn;
                                break;
                            }
                            block = *self
                                .db
                                .get_entry_by_hash(block.prev_block)?
                                .expect("earlier block should exist");
                        }
                    }
                }
                ThresholdState::LockedIn => {
                    state = ThresholdState::Active;
                }
                ThresholdState::Failed | ThresholdState::Active => (),
            }

            self.db.version_bits_cache.set(bit, entry.hash, state);
        }

        Ok(state)
    }

    fn get_deployment_state(&mut self) -> Result<DeploymentState, Error> {
        let prev = self.db.get_entry_by_hash(self.tip.prev_block)?;
        let prev = if let Some(prev) = prev {
            *prev
        } else {
            assert!(self.tip.is_genesis());
            return Ok(self.state);
        };
        self.get_deployments(self.tip.time, prev)
    }

    fn get_deployments(&mut self, time: u32, prev: ChainEntry) -> Result<DeploymentState, Error> {
        let deployments = &self.options.network.deployments;
        let taproot = deployments["taproot"];
        let ctv = deployments["ctv"];

        let height = prev.height + 1;
        let mut state = DeploymentState::default();

        if time >= self.options.network.bip16_time {
            state.script_flags |= ScriptFlags::VERIFY_P2SH;
        }

        if height >= self.options.network.bip34_height {
            state.bip34 = true;
        }

        if height >= self.options.network.bip66_height {
            state.script_flags |= ScriptFlags::VERIFY_DERSIG;
        }

        if height >= self.options.network.bip65_height {
            state.script_flags |= ScriptFlags::VERIFY_CHECKLOCKTIMEVERIFY;
        }

        if height >= self.options.network.csv_height {
            state.script_flags |= ScriptFlags::VERIFY_CHECKSEQUENCEVERIFY;
            state.lock_flags |= LockFlags::VERIFY_SEQUENCE;
            state.lock_flags |= LockFlags::MEDIAN_TIME_PAST;
        }

        if height >= self.options.network.segwit_height {
            state.script_flags |= ScriptFlags::VERIFY_WITNESS;
            state.script_flags |= ScriptFlags::VERIFY_NULLDUMMY;
        }

        if self.is_deployment_active(prev, taproot)? {
            state.script_flags |= ScriptFlags::VERIFY_TAPROOT;
        }

        if self.is_deployment_active(prev, ctv)? {
            state.script_flags |= ScriptFlags::VERIFY_STANDARD_TEMPLATE;
        }

        Ok(state)
    }

    fn is_deployment_active(
        &mut self,
        prev: ChainEntry,
        deployment: BIP9Deployment,
    ) -> Result<bool, Error> {
        let status = self.get_deployment_status(prev, deployment)?;
        Ok(status == ThresholdState::Active)
    }

    fn verify_locks(
        &mut self,
        prev: &ChainEntry,
        tx: &Transaction,
        view: &CoinView,
        flags: &LockFlags,
    ) -> bool {
        let (height, time) = self.get_locks(&prev, tx, view, flags);

        if let Some(height) = height {
            if height >= prev.height + 1 {
                return false;
            }
        }

        if let Some(time) = time {
            let mtp = self.get_median_time(*prev).unwrap();

            if time >= mtp {
                return false;
            }
        }

        true
    }

    fn get_locks(
        &mut self,
        prev: &ChainEntry,
        tx: &Transaction,
        view: &CoinView,
        flags: &LockFlags,
    ) -> (Option<u32>, Option<u32>) {
        use crate::protocol::consensus::{
            SEQUENCE_DISABLE_FLAG, SEQUENCE_GRANULARITY, SEQUENCE_MASK, SEQUENCE_TYPE_FLAG,
        };

        if !flags.contains(LockFlags::VERIFY_SEQUENCE) {
            return (None, None);
        }

        if tx.is_coin_base() || tx.version < 2 {
            return (None, None);
        }

        let mut min_height = None;
        let mut min_time = None;

        for input in &tx.input {
            if input.sequence & SEQUENCE_DISABLE_FLAG != 0 {
                continue;
            }

            let mut height: u32 = view
                .get_entry(&input.previous_output)
                .and_then(|entry| entry.height)
                .unwrap_or(self.height + 1);

            if input.sequence & SEQUENCE_TYPE_FLAG == 0 {
                height += (input.sequence & SEQUENCE_MASK) - 1;
                min_height = Some(std::cmp::max(min_height.unwrap_or(0), height));
                continue;
            }

            height = height.checked_sub(1).unwrap_or(0);

            let entry = self
                .db
                .get_ancestor(prev, height)
                .expect("prev must exist before next checked");

            let mut time = self.get_median_time(entry).unwrap();
            time += ((input.sequence & SEQUENCE_MASK) << SEQUENCE_GRANULARITY) - 1;
            min_time = Some(std::cmp::max(min_time.unwrap_or(0), time));
        }

        (min_height, min_time)
    }

    // fn notify_block(&self, block: &Block, entry: &ChainEntry) {
    //     for listener in &self.listeners {
    //         listener.handle_block(block, entry);
    //     }
    // }

    // fn notify_tip(&self, tip: &ChainEntry) {
    //     for listener in &self.listeners {
    //         listener.handle_tip(tip);
    //     }
    // }

    // fn notify_connect(&self, entry: &ChainEntry, block: &Block, view: &CoinView) {
    //     for listener in &self.listeners {
    //         listener.handle_connect(entry, block, view);
    //     }
    // }

    pub fn add_listener(&mut self, listener: Arc<dyn ChainListener>) {
        self.listeners.push(listener);
    }
}

pub struct ChainOptions {
    pub network: NetworkParams,
    pub path: PathBuf,
}

fn ms_since(start: &SystemTime) -> f32 {
    start.elapsed().unwrap().as_secs_f32() * 1000.0
}

#[derive(Default, Copy, Clone)]
pub struct DeploymentState {
    pub script_flags: ScriptFlags,
    lock_flags: LockFlags,
    bip34: bool,
}

impl DeploymentState {
    pub fn has_p2sh(&self) -> bool {
        self.script_flags.contains(ScriptFlags::VERIFY_P2SH)
    }

    pub fn has_bip34(&self) -> bool {
        self.bip34
    }

    pub fn has_bip66(&self) -> bool {
        self.script_flags.contains(ScriptFlags::VERIFY_DERSIG)
    }

    pub fn has_cltv(&self) -> bool {
        self.script_flags
            .contains(ScriptFlags::VERIFY_CHECKLOCKTIMEVERIFY)
    }

    pub fn has_mtp(&self) -> bool {
        self.lock_flags.contains(LockFlags::MEDIAN_TIME_PAST)
    }

    pub fn has_csv(&self) -> bool {
        self.script_flags
            .contains(ScriptFlags::VERIFY_CHECKSEQUENCEVERIFY)
    }

    pub fn has_witness(&self) -> bool {
        self.script_flags.contains(ScriptFlags::VERIFY_WITNESS)
    }

    pub fn has_ctv(&self) -> bool {
        self.script_flags
            .contains(ScriptFlags::VERIFY_STANDARD_TEMPLATE)
    }

    pub fn has_taproot(&self) -> bool {
        self.script_flags.contains(ScriptFlags::VERIFY_TAPROOT)
    }
}
