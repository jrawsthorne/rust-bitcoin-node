use super::{chain_db::get_skip_height, ChainDB, ChainDBOptions, ChainEntry};
use crate::coins::CoinView;
use crate::protocol::{
    consensus::{self, LockFlags, ScriptFlags},
    BIP8Deployment, BIP8ThresholdState, BIP9Deployment, BIP9ThresholdState, Deployment,
    NetworkParams, StartTime, ThresholdState, Timeout, VERSIONBITS_TOP_BITS,
};
use crate::{
    error::{
        BlockHeaderVerificationError, BlockVerificationError, DBError, TransactionVerificationError,
    },
    primitives::{BlockExt, TransactionExt},
    util::{ms_since, now},
};
use bitcoin::{
    blockdata::constants::{
        genesis_block, DIFFCHANGE_INTERVAL, DIFFCHANGE_TIMESPAN, MAX_BLOCK_WEIGHT,
    },
    util::uint::Uint256,
    Block, BlockHash, BlockHeader, Network, Transaction,
};
use log::{error, info, warn};
use rayon::prelude::*;
use std::collections::VecDeque;
use std::{path::PathBuf, time::Instant};

pub trait ChainListener: Send + Sync {
    fn handle_connect(
        &self,
        _chain: &Chain,
        _entry: &ChainEntry,
        _block: &Block,
        _view: &CoinView,
    ) {
    }

    fn handle_disconnect(
        &self,
        _chain: &Chain,
        _entry: &ChainEntry,
        _block: &bitcoin::Block,
        _view: &CoinView,
    ) {
    }
}

pub struct Chain {
    pub db: ChainDB,
    pub tip: ChainEntry,
    pub height: u32,
    pub options: ChainOptions,
    pub state: DeploymentState,
    listeners: Vec<Box<dyn ChainListener>>,
}

impl Chain {
    pub fn new(options: ChainOptions) -> Result<Self, DBError> {
        let mut chain = Self {
            db: ChainDB::new(
                options.network.clone(),
                ChainDBOptions {
                    path: options.path.clone(),
                },
            ),
            tip: ChainEntry::default(),
            height: 0,
            options,
            state: DeploymentState::default(),
            listeners: vec![],
        };

        chain.db.open()?;

        let tip = *chain
            .db
            .get_tip()
            .expect("always tip if chain db opened first");

        chain.height = tip.height;
        chain.tip = tip;

        info!("Chain Height: {}", chain.tip.height);

        chain.state = chain.get_deployment_state();

        Ok(chain)
    }

    pub fn height_to_hash_range(
        &self,
        start_height: u32,
        end_hash: &BlockHash,
        max_results: usize,
    ) -> Vec<BlockHash> {
        let end = self.db.get_entry_by_hash(end_hash).unwrap();
        let end_height = end.height;

        assert!(start_height < end_height);

        let results_len = end_height - start_height + 1;

        assert!(results_len as usize <= max_results);

        let mut hashes = VecDeque::new();

        let mut entry = end;
        for _ in 0..results_len {
            hashes.push_front(entry.hash);
            entry = self.db.get_entry_by_hash(&entry.prev_block).unwrap();
        }

        hashes.into()
    }

    pub fn add_listener(&mut self, listener: impl ChainListener + 'static) {
        self.listeners.push(Box::new(listener));
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

    fn is_block_ready(&self, hash: BlockHash) -> bool {
        !self.db.has_invalid(&hash) && self.db.has_block(hash)
    }

    /// Add a block to the chain and return a chain entry representing it if it was added successfully
    pub fn add(&mut self, block: Block) -> Result<ChainEntry, BlockVerificationError> {
        let (entry, prev) = match self.db.get_entry_by_hash(&block.block_hash()) {
            Some(&entry) => {
                let prev = *self
                    .db
                    .get_entry_by_hash(&block.header.prev_blockhash)
                    .expect("prev block to be added before this");
                (entry, prev)
            }
            None => self.add_header(&block.header)?,
        };

        self.add_block(&block, Some(entry), Some(&prev))?;
        self.connect_best(entry, prev, Some(block))?;

        Ok(entry)
    }

    fn add_block(
        &mut self,
        block: &Block,
        entry: Option<ChainEntry>,
        prev: Option<&ChainEntry>,
    ) -> Result<(), BlockVerificationError> {
        let hash = block.block_hash();

        let entry = if let Some(entry) = entry {
            entry
        } else {
            *self
                .db
                .get_entry_by_hash(&hash)
                .expect("block only added if we have header")
        };

        let prev = if let Some(&prev) = prev {
            prev
        } else {
            *self
                .db
                .get_entry_by_hash(&block.header.prev_blockhash)
                .expect("block only added if we have previous header")
        };

        assert!(!self.db.has_block(hash));

        if let Err(err) = self.verify(block, prev) {
            // check if mutated, don't add to invalid set if mutated
            self.db.set_invalid(entry.hash);
            return Err(err);
        }

        self.db.write_block(block).unwrap();

        if entry.chainwork <= self.tip.chainwork {
            warn!("Heads up: Competing chain at height {}: tip-height={} competitor-height={} tip-hash={} competitor-hash={} tip-chainwork={} competitor-chainwork={} chainwork-diff={}", entry.height, self.tip.height, entry.height, self.tip.hash, entry.hash, self.tip.chainwork, entry.chainwork, self.tip.chainwork - entry.chainwork);
        }

        Ok(())
    }

    pub fn find_locator(&self, locator: &[BlockHash]) -> BlockHash {
        for hash in locator {
            if self.db.is_main_hash(hash) {
                return *hash;
            }
        }

        genesis_block(self.options.network.network).block_hash()
    }

    // should be one for one match with `AcceptBlockHeader` in core
    pub fn add_header(
        &mut self,
        header: &BlockHeader,
    ) -> Result<(ChainEntry, ChainEntry), BlockHeaderVerificationError> {
        header
            .validate_pow(&header.target())
            .map_err(|_| BlockHeaderVerificationError::InvalidPOW)?;

        let hash = header.block_hash();

        assert!(!self.has_invalid(hash, header.prev_blockhash));

        assert!(!self.db.has_header(&hash));

        let prev = *self
            .db
            .get_entry_by_hash(&header.prev_blockhash)
            .expect("should have previous header before adding this");

        let mut entry = ChainEntry::from_block_header(header, Some(&prev));
        if !entry.is_genesis() {
            let skip = self
                .db
                .get_ancestor(&prev, get_skip_height(entry.height))
                .hash;
            entry.skip = skip;
        }

        if let Err(err) = self.contextual_check_block_header(header, &prev) {
            // TODO: check for mutation and set invalid if not mutated
            return Err(err);
        };

        self.db.save_entry(entry, &prev).unwrap();

        self.log_headers(&entry);

        Ok((entry, prev))
    }

    fn log_headers(&self, entry: &ChainEntry) {
        if entry.height % 2000 == 0 || self.is_recent() {
            info!(
                "Headers Status: hash={} height={}",
                entry.hash, entry.height,
            );
        }
    }

    pub fn attach(&mut self, entry: ChainEntry) -> Result<(), BlockVerificationError> {
        if self.db.is_main_chain(&entry) {
            return Ok(());
        }

        let prev = *self
            .db
            .get_entry_by_hash(&entry.prev_block)
            .expect("prev already checked to exist before attach");

        self.connect_best(entry, prev, None)?;

        Ok(())
    }

    // TODO: Completely change the way this is done. Core keeps best candidate block using map of block -> parent
    fn get_next_best(&self, entry: ChainEntry) -> Result<VecDeque<ChainEntry>, DBError> {
        fn get_next_best(
            chain: &Chain,
            entry: ChainEntry,
        ) -> Result<VecDeque<ChainEntry>, DBError> {
            let mut max_work = Uint256::from_u64(0).unwrap();
            let mut max_path = VecDeque::new();

            if !chain.is_block_ready(entry.hash) {
                return Ok(max_path);
            }

            let entries = chain.db.get_next_entries(entry.hash)?;

            for next in entries {
                let path = get_next_best(chain, next)?;
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

        let mut path = get_next_best(self, entry)?;
        if !path.is_empty() {
            path.pop_front();
        }
        Ok(path)
    }

    fn connect_best(
        &mut self,
        mut entry: ChainEntry,
        mut prev: ChainEntry,
        block: Option<Block>,
    ) -> Result<(), BlockVerificationError> {
        let _tip = self.tip;

        assert!(entry.prev_block == prev.hash);

        let mut queue = VecDeque::new();

        if self.is_block_ready(entry.hash) {
            queue.push_back((entry, prev, block));

            let mut head;
            let mut head_prev = prev;

            while !self.db.is_main_chain(&head_prev) {
                if self.is_block_ready(head_prev.hash) {
                    head = head_prev;
                    head_prev = *self.db.get_entry_by_hash(&head.prev_block).unwrap();
                    queue.push_front((head, head_prev, None));
                } else {
                    return Ok(());
                }
            }

            // See if there are any blocks following
            // that are waiting to be connected.
            let nexts = self.get_next_best(entry).unwrap();
            for next in nexts {
                prev = entry;
                entry = next;
                queue.push_back((entry, prev, None));
            }

            // Verify that this new set of entries has greater
            // work than the current chain.

            let (last_entry, _, _) = queue[queue.len() - 1];
            if last_entry.chainwork <= self.tip.chainwork {
                queue.clear();
            }
        }

        if queue.is_empty() {
            return Ok(());
        }

        let _reorg = self.tip.hash != queue[0].0.prev_block;

        while self.tip.hash != queue[0].0.prev_block {
            self.disconnect(self.tip).unwrap();
        }

        // let common = self.tip;

        // TODO: Handle errors

        for (entry, prev, block) in queue {
            let block = if let Some(block) = block {
                block
            } else {
                self.db.get_block(entry.hash).unwrap().unwrap()
            };
            self.connect(&block, entry, prev)?;
        }

        Ok(())
    }

    fn disconnect(&mut self, entry: ChainEntry) -> Result<(), DBError> {
        let block = self.db.get_block(entry.hash)?.expect("block not found");

        let prev = *self
            .db
            .get_entry_by_hash(&entry.prev_block)
            .expect("prev not found");

        let view = self.db.disconnect(entry, &block)?;

        self.tip = prev;
        self.height = prev.height;

        self.notify_disconnect(&entry, &block, &view);

        Ok(())
    }

    // should be one for one match with core `ContextualCheckBlockHeader`
    fn contextual_check_block_header(
        &self,
        header: &BlockHeader,
        prev: &ChainEntry,
    ) -> Result<(), BlockHeaderVerificationError> {
        assert_eq!(header.prev_blockhash, prev.hash);

        // check block bits are correct and retarget if necessary
        let bits = self.get_target(header.time, Some(prev));
        if bits != header.bits {
            return Err(BlockHeaderVerificationError::BadDifficultyBits);
        }

        let mtp = self.get_median_time(prev);
        if header.time <= mtp {
            return Err(BlockHeaderVerificationError::TimeTooOld);
        }

        if (header.time as u64) >= now() + consensus::MAX_FUTURE_BLOCK_TIME as u64 {
            return Err(BlockHeaderVerificationError::TimeTooNew);
        }

        let height = prev.height;

        // Reject outdated version blocks when 95% (75% on testnet) of the network has upgraded:
        // check for version 2, 3 and 4 upgrades
        if header.version < 2 && height >= self.options.network.bip34_height
            || header.version < 3 && height >= self.options.network.bip66_height
            || header.version < 4 && height >= self.options.network.bip65_height
        {
            return Err(BlockHeaderVerificationError::Obsolete);
        }

        Ok(())
    }

    pub fn common_ancestor<'a>(
        &'a self,
        mut a: &'a ChainEntry,
        mut b: &'a ChainEntry,
    ) -> Option<&ChainEntry> {
        if a.height > b.height {
            a = self.db.get_ancestor(a, b.height);
        } else if b.height > a.height {
            b = self.db.get_ancestor(b, a.height);
        }

        let mut a = Some(a);
        let mut b = Some(b);

        loop {
            match (a, b) {
                (Some(aa), Some(bb)) if aa.hash != bb.hash => {
                    a = self.db.get_entry_by_hash(&aa.prev_block);
                    b = self.db.get_entry_by_hash(&bb.prev_block);
                }
                (Some(_), Some(_)) => break a,
                _ => break None,
            }
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

    pub fn get_locator(&self, start: Option<BlockHash>) -> Vec<BlockHash> {
        let start = start.unwrap_or(self.tip.hash);
        let mut hashes = vec![];

        let start_entry = match self.db.get_entry_by_hash(&start) {
            Some(entry) => *entry,
            None => {
                hashes.push(start);
                self.tip
            }
        };

        let mut in_best_chain = self.db.is_main_chain(&start_entry);
        let mut hash = start_entry.hash;
        let mut height = start_entry.height;
        let mut step = 1;

        hashes.push(hash);

        while height > 0 {
            height = height.saturating_sub(step);

            if hashes.len() > 10 {
                step *= 2;
            }

            if in_best_chain {
                hash = self
                    .db
                    .get_entry_by_height(height)
                    .expect("main chain so must exist by height")
                    .hash;
            } else {
                let ancestor = self.db.get_ancestor(&start_entry, height);
                in_best_chain = self.db.is_main_chain(&ancestor);
                hash = ancestor.hash;
            }

            hashes.push(hash);
        }

        hashes
    }

    pub fn is_recent(&self) -> bool {
        let best = self.most_work();
        let time = crate::util::now().saturating_sub(self.options.network.max_tip_age as u64);

        best.time > time as u32
    }

    pub fn synced(&self) -> bool {
        let time = crate::util::now().saturating_sub(self.options.network.max_tip_age as u64);

        self.tip.time > time as u32
    }

    pub fn most_work(&self) -> &ChainEntry {
        self.db.most_work.as_ref().expect("chain db to be open")
    }

    fn connect(
        &mut self,
        block: &Block,
        entry: ChainEntry,
        prev: ChainEntry,
    ) -> Result<ChainEntry, BlockVerificationError> {
        let start = Instant::now();

        assert_eq!(block.header.prev_blockhash, prev.hash);
        assert_eq!(prev.hash, self.tip.hash);

        let (view, state) = match self.verify_context(block, prev) {
            Ok(res) => res,
            Err(err) => {
                // if let ChainError::VerificationError = err {
                self.db.set_invalid(entry.hash);
                error!(
                    "Tried to connect invalid block: {} ({}).",
                    entry.hash, entry.height
                );
                // }
                return Err(err);
            }
        };

        self.db.connect(entry, block, &view).unwrap();

        self.height = entry.height;
        self.tip = entry;

        self.set_deployment_state(state);

        self.notify_connect(&entry, block, &view);

        info!(
            "Block {} ({}) added to chain (size={} txs={} time={}ms progress={:.2}%)",
            entry.hash,
            entry.height,
            block.get_size(),
            block.txdata.len(),
            ms_since(&start),
            (self.db.state.tx as f64 / self.options.network.expected_tx_count as f64) * 100.0,
        );

        Ok(entry)
    }

    fn verify_context(
        &mut self,
        block: &Block,
        prev: ChainEntry,
    ) -> Result<(CoinView, DeploymentState), BlockVerificationError> {
        let state = self.get_deployments(block.header.time, prev);

        // non contextual verification;
        self.verify(&block, prev)?;

        // verify duplicate txids
        if !state.has_bip34() || prev.height + 1 >= consensus::BIP34_IMPLIES_BIP30_LIMIT {
            self.verify_duplicates(block, &prev)?;
        }

        // do full verification
        let view = self.verify_inputs(block, &prev, &state)?;

        Ok((view, state))
    }

    fn verify_duplicates(
        &self,
        block: &Block,
        prev: &ChainEntry,
    ) -> Result<(), TransactionVerificationError> {
        use TransactionVerificationError::DuplicateTxid;
        block
            .txdata
            .par_iter()
            .map(|tx| {
                if !self.db.has_coins(tx).unwrap() {
                    return Ok(());
                }

                let height = prev.height + 1;
                let hash = self.options.network.bip30.get(&height);

                match hash {
                    Some(hash) if *hash != block.block_hash() => Err(DuplicateTxid),
                    None => Err(DuplicateTxid),
                    _ => Ok(()),
                }
            })
            .collect::<Result<_, _>>()
    }

    fn verify_inputs(
        &self,
        block: &Block,
        prev: &ChainEntry,
        state: &DeploymentState,
    ) -> Result<CoinView, BlockVerificationError> {
        let mut view = CoinView::default();

        let height = prev.height + 1;

        let mut sigops = 0;
        let mut reward: u64 = 0;

        for tx in &block.txdata {
            if !tx.is_coin_base() {
                view.spend_inputs(&self.db, tx)?;
            }
            if !tx.is_coin_base() {
                let fee = tx.check_inputs(&view, height)?;
                reward = reward
                    .checked_add(fee)
                    .ok_or_else(|| TransactionVerificationError::InputValuesOutOfRange)?;
                if reward > consensus::MAX_MONEY {
                    return Err(TransactionVerificationError::FeeOutOfRange)?;
                }
            }
            if !tx.is_coin_base() && tx.version >= 2 {
                self.verify_locks(prev, tx, &view, &state.lock_flags)?;
            }
            sigops += tx.get_sigop_cost(&view, state.script_flags);
            if sigops > consensus::MAX_BLOCK_SIGOPS_COST {
                return Err(BlockVerificationError::BadSigops);
            }

            view.add_tx(tx, height);
        }

        reward += consensus::get_block_subsidy(height, &self.options.network);

        if block.get_claimed() > reward {
            return Err(BlockVerificationError::BadCoinbaseAmount);
        }

        if self.options.verify_scripts {
            use rayon::prelude::*;

            block
                .txdata
                .par_iter()
                .skip(1) // skip coinbase as the miner can choose any script they want
                .map(|tx| tx.verify_scripts(&view, &state.script_flags))
                .collect::<Result<_, _>>()?;
        }

        Ok(view)
    }

    pub fn get_median_time(&self, prev: &ChainEntry) -> u32 {
        let mut median = [0u32; consensus::MEDIAN_TIMESPAN];
        let mut entry = Some(prev);

        let mut end = 0;

        for _ in 0..consensus::MEDIAN_TIMESPAN {
            if let Some(chain_entry) = entry {
                median[end] = chain_entry.time;
                entry = self.db.get_entry_by_hash(&chain_entry.prev_block);
                end += 1;
            } else {
                break;
            }
        }

        let median = &mut median[..end];

        median.sort_unstable();

        median[median.len() / 2]
    }

    fn verify(&mut self, block: &Block, prev: ChainEntry) -> Result<(), BlockVerificationError> {
        assert_eq!(block.header.prev_blockhash, prev.hash);

        // non contextual block verification
        block.check_body()?;

        let height = prev.height + 1;

        let state = self.get_deployments(block.header.time, prev);

        let mtp = self.get_median_time(&prev);
        let time = if state.has_mtp() {
            mtp
        } else {
            block.header.time
        };

        // Transactions must be finalized with
        // regards to nSequence and nLockTime.
        let all_final = block.txdata.par_iter().all(|tx| {
            // TODO: i32 -> Option
            tx.is_final(height, time as i32)
        });

        if !all_final {
            return Err(TransactionVerificationError::NonFinal)?;
        }

        // bip34 made coinbase txs unique by including the height
        // of the block in the scriptsig
        if state.has_bip34() {
            block.check_coinbase_height(prev.height + 1)?;
        }

        // Check witness commitment hash.
        // Returns true if block is not segwit so always check
        if !block.check_witness_commitment() {
            return Err(BlockVerificationError::BadWitnessCommitment);
        }

        // check block weight
        if block.get_weight() > MAX_BLOCK_WEIGHT as usize {
            return Err(BlockVerificationError::BadBlockWeight);
        }

        Ok(())
    }

    pub fn get_target(&self, time: u32, prev: Option<&ChainEntry>) -> u32 {
        let pow_limit_bits = self.options.network.pow_limit_bits;

        let prev = match prev {
            Some(prev) => prev,
            None => {
                return pow_limit_bits;
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
                    return pow_limit_bits;
                }
                while prev.height != 0
                    && prev.height % DIFFCHANGE_INTERVAL != 0
                    && prev.bits == pow_limit_bits
                {
                    prev = *self.db.get_entry_by_hash(&prev.prev_block).unwrap();
                }
            }
            return prev.bits;
        }

        assert!(prev.height >= DIFFCHANGE_INTERVAL - 1);

        // Go back by what we want to be 14 days worth of blocks
        let height = prev.height - (DIFFCHANGE_INTERVAL - 1);

        let first = self.db.get_ancestor(prev, height);

        self.retarget(prev, &first)
    }

    fn retarget(&self, prev: &ChainEntry, first: &ChainEntry) -> u32 {
        // don't retarget on regtest
        if self.options.network.no_pow_retargeting {
            return prev.bits;
        }

        let pow_limit = self.options.network.pow_limit;
        let pow_limit_bits = self.options.network.pow_limit_bits;

        let mut target = BlockHeader::u256_from_compact_target(prev.bits);

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
            return pow_limit_bits;
        }

        BlockHeader::compact_target_from_u256(&target)
    }

    pub fn compute_block_version(&mut self, prev: ChainEntry) -> i32 {
        let mut version = 0;

        for deployment in self.options.network.deployments.clone().values() {
            let state = self.get_state(prev, *deployment);

            if state.is_locked_in() || state.is_started() {
                version |= 1 << deployment.bit();
            }
        }

        version |= VERSIONBITS_TOP_BITS;
        version >>= 0;

        version
    }

    pub fn get_bip8_deployment_status(
        &mut self,
        mut prev: Option<ChainEntry>,
        deployment: BIP8Deployment,
    ) -> BIP8ThresholdState {
        let bit = deployment.bit;

        let start_height = deployment.start_height;
        let timeout_height = deployment.timeout_height;
        let lock_in_on_timeout = deployment.lock_in_on_timeout;
        let threshold = self.options.network.rule_change_activation_threshold;
        let window = self.options.network.miner_confirmation_window;

        if let Some(entry) = prev {
            let height = entry.height.checked_sub((entry.height + 1) % window);
            if let Some(height) = height {
                prev = Some(*self.db.get_ancestor(&entry, height));
            } else {
                prev = None;
            }
        }

        let mut to_compute = vec![];
        let mut prev_hash;

        loop {
            prev_hash = prev.map(|p| p.hash).unwrap_or_default();

            // Already cached, don't need to compute any further back
            if self.db.version_bits_cache.get_8(bit, &prev_hash).is_some() {
                break;
            }

            match prev {
                None => {
                    self.db.version_bits_cache.set(
                        bit,
                        prev_hash,
                        ThresholdState::BIP8(BIP8ThresholdState::Defined),
                    );
                    break;
                }
                Some(entry) => {
                    if entry.height + 1 < start_height {
                        self.db.version_bits_cache.set(
                            bit,
                            prev_hash,
                            ThresholdState::BIP8(BIP8ThresholdState::Defined),
                        );
                        break;
                    }
                    to_compute.push(entry);
                    if let Some(height) = entry.height.checked_sub(window) {
                        prev = Some(*self.db.get_ancestor(&entry, height));
                    } else {
                        prev = None;
                    }
                }
            }
        }

        let mut state = *self.db.version_bits_cache.get_8(bit, &prev_hash).unwrap();

        for entry in to_compute {
            let height = entry.height + 1;
            match state {
                BIP8ThresholdState::Defined => {
                    if height >= start_height {
                        state = BIP8ThresholdState::Started;
                    }
                }
                BIP8ThresholdState::Started => {
                    let mut block = entry;
                    let mut count = 0;

                    for _ in 0..window {
                        if block.has_bit(bit) {
                            count += 1;
                        }
                        block = *self
                            .db
                            .get_entry_by_hash(&block.prev_block)
                            .expect("earlier block should exist");
                    }

                    if count >= threshold {
                        state = BIP8ThresholdState::LockedIn;
                    } else if height >= timeout_height {
                        state = if lock_in_on_timeout {
                            BIP8ThresholdState::LockedIn
                        } else {
                            BIP8ThresholdState::Failing
                        };
                    }
                }
                BIP8ThresholdState::Failing => {
                    // Only if every block signals does this become ACTIVE
                    state = BIP8ThresholdState::Active;
                    let mut block = entry;
                    for _ in 0..window {
                        if !block.has_bit(bit) {
                            state = BIP8ThresholdState::Failed;
                            break;
                        }
                        block = *self
                            .db
                            .get_entry_by_hash(&block.prev_block)
                            .expect("earlier block should exist");
                    }
                }
                BIP8ThresholdState::LockedIn => {
                    state = BIP8ThresholdState::Active;
                }
                BIP8ThresholdState::Active | BIP8ThresholdState::Failed => {}
            }

            self.db
                .version_bits_cache
                .set(bit, entry.hash, ThresholdState::BIP8(state));
        }

        state
    }

    fn get_bip9_deployment_status(
        &mut self,
        mut prev: ChainEntry,
        deployment: BIP9Deployment,
    ) -> BIP9ThresholdState {
        let bit = deployment.bit;

        let start_time = match deployment.start_time {
            StartTime::AlwaysActive => return BIP9ThresholdState::Active,
            StartTime::StartTime(start_time) => start_time,
        };

        let timeout = deployment.timeout;
        let window = self.options.network.miner_confirmation_window;
        let threshold = self.options.network.rule_change_activation_threshold;

        // All blocks within a retarget period have the same state
        if ((prev.height + 1) % window) != 0 {
            let height = prev.height.checked_sub((prev.height + 1) % window);
            if let Some(height) = height {
                prev = *self.db.get_ancestor(&prev, height);

                assert!(prev.height == height);
                assert!(((prev.height + 1) % window) == 0);
            } else {
                return BIP9ThresholdState::Defined;
            }
        }

        let mut entry = prev;
        let mut state = BIP9ThresholdState::Defined;

        let mut compute = vec![];

        loop {
            if let Some(cached) = self.db.version_bits_cache.get_9(bit, &entry.hash) {
                state = *cached;
                break;
            }
            let time = self.get_median_time(&entry);
            if time < start_time {
                state = BIP9ThresholdState::Defined;
                self.db
                    .version_bits_cache
                    .set(bit, entry.hash, ThresholdState::BIP9(state));
                break;
            }
            compute.push(entry);
            let height = entry.height.checked_sub(window);
            if let Some(height) = height {
                entry = *self.db.get_ancestor(&entry, height);
            } else {
                break;
            }
        }

        for entry in compute {
            match state {
                BIP9ThresholdState::Defined => {
                    let time = self.get_median_time(&entry);

                    match timeout {
                        Timeout::Timeout(timeout) if time >= timeout => {
                            state = BIP9ThresholdState::Failed;
                        }
                        _ => {
                            if time >= start_time {
                                state = BIP9ThresholdState::Started;
                            }
                        }
                    }
                }
                BIP9ThresholdState::Started => {
                    let time = self.get_median_time(&entry);

                    match timeout {
                        Timeout::Timeout(timeout) if time >= timeout => {
                            state = BIP9ThresholdState::Failed;
                        }
                        _ => {
                            let mut block = entry;
                            let mut count = 0;
                            for _ in 0..window {
                                if block.has_bit(bit) {
                                    count += 1;
                                }
                                if count >= threshold {
                                    state = BIP9ThresholdState::LockedIn;
                                    break;
                                }
                                block = *self
                                    .db
                                    .get_entry_by_hash(&block.prev_block)
                                    .expect("earlier block should exist");
                            }
                        }
                    }
                }
                BIP9ThresholdState::LockedIn => state = BIP9ThresholdState::Active,
                BIP9ThresholdState::Failed | BIP9ThresholdState::Active => (),
            }

            self.db
                .version_bits_cache
                .set(bit, entry.hash, ThresholdState::BIP9(state));
        }

        state
    }

    pub fn get_state(&mut self, prev: ChainEntry, deployment: Deployment) -> ThresholdState {
        match deployment {
            Deployment::BIP8(deployment) => {
                ThresholdState::BIP8(self.get_bip8_deployment_status(Some(prev), deployment))
            }
            Deployment::BIP9(deployment) => {
                ThresholdState::BIP9(self.get_bip9_deployment_status(prev, deployment))
            }
        }
    }

    pub fn get_deployment_state(&mut self) -> DeploymentState {
        let prev = self.db.get_entry_by_hash(&self.tip.prev_block);
        let prev = if let Some(prev) = prev {
            *prev
        } else {
            assert!(self.tip.is_genesis());
            return self.state;
        };
        self.get_deployments(self.tip.time, prev)
    }

    fn get_deployments(&mut self, time: u32, prev: ChainEntry) -> DeploymentState {
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

        if self.is_deployment_active(prev, taproot) {
            state.script_flags |= ScriptFlags::VERIFY_TAPROOT;
        }

        if self.is_deployment_active(prev, ctv) {
            state.script_flags |= ScriptFlags::VERIFY_STANDARD_TEMPLATE;
        }

        state
    }

    fn is_deployment_active(&mut self, prev: ChainEntry, deployment: Deployment) -> bool {
        match deployment {
            Deployment::BIP8(deployment) => {
                let status = self.get_bip8_deployment_status(Some(prev), deployment);
                status == BIP8ThresholdState::Active
            }
            Deployment::BIP9(deployment) => {
                let status = self.get_bip9_deployment_status(prev, deployment);
                status == BIP9ThresholdState::Active
            }
        }
    }

    pub fn verify_locks(
        &self,
        prev: &ChainEntry,
        tx: &Transaction,
        view: &CoinView,
        flags: &LockFlags,
    ) -> Result<(), TransactionVerificationError> {
        let (height, time) = self.get_locks(&prev, tx, view, flags);

        if let Some(height) = height {
            if height >= prev.height + 1 {
                return Err(TransactionVerificationError::NonFinal);
            }
        }

        if let Some(time) = time {
            let mtp = self.get_median_time(prev);

            if time >= mtp {
                return Err(TransactionVerificationError::NonFinal);
            }
        }

        Ok(())
    }

    fn get_locks(
        &self,
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

            let entry = self.db.get_ancestor(prev, height);

            let mut time = self.get_median_time(entry);
            time += ((input.sequence & SEQUENCE_MASK) << SEQUENCE_GRANULARITY) - 1;
            min_time = Some(std::cmp::max(min_time.unwrap_or(0), time));
        }

        (min_height, min_time)
    }

    pub fn verify_final(&self, prev: &ChainEntry, tx: &Transaction, flags: &LockFlags) -> bool {
        let height = prev.height + 1;

        if tx.lock_time < consensus::LOCKTIME_THRESHOLD {
            return tx.is_final(height, -1);
        }

        if flags.contains(LockFlags::MEDIAN_TIME_PAST) {
            let time = self.get_median_time(prev);
            return tx.is_final(height, time as i32);
        }

        tx.is_final(height, now() as i32)
    }

    fn notify_connect(&self, entry: &ChainEntry, block: &Block, view: &CoinView) {
        for listener in &self.listeners {
            listener.handle_connect(self, entry, block, view);
        }
    }

    fn notify_disconnect(&self, entry: &ChainEntry, block: &Block, view: &CoinView) {
        for listener in &self.listeners {
            listener.handle_disconnect(self, entry, block, view);
        }
    }
}

pub struct ChainOptions {
    pub network: NetworkParams,
    pub path: PathBuf,
    pub verify_scripts: bool,
}

impl ChainOptions {
    pub fn new(network: Network, path: PathBuf, verify_scripts: bool) -> Self {
        Self {
            network: NetworkParams::from_network(network),
            path,
            verify_scripts,
        }
    }
}

#[derive(Default, Copy, Clone, Debug)]
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
