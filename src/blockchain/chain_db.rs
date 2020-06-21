use super::ChainEntry;
use crate::blockstore::{BlockStore, BlockStoreOptions};
use crate::coins::{CoinEntry, CoinView};
use crate::db::{
    Batch, Database, DiskDatabase, Iter, IterMode, Key, COL_CHAIN_ENTRY, COL_CHAIN_ENTRY_HASH,
    COL_CHAIN_WORK, COL_VERSION_BIT_STATE,
};
use crate::error::DBError;
use crate::protocol::{NetworkParams, ThresholdState};
use bitcoin::Amount;
use bitcoin::{
    consensus::{
        encode::{deserialize, serialize},
        Decodable,
    },
    Block, BlockHash, OutPoint, Transaction, TxOut,
};
use log::info;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Default)]
pub struct ChainEntryCache {
    height: HashMap<u32, BlockHash>,
    hash: HashMap<BlockHash, ChainEntry>,
    batch: Vec<ChainEntryCacheOperation>,
}

enum ChainEntryCacheOperation {
    InsertHash(ChainEntry),
    InsertHeight(ChainEntry),
    // RemoveHash(BlockHash),
    // RemoveHeight(u32),
}

impl ChainEntryCache {
    pub fn new(db: &DiskDatabase) -> Self {
        let mut cache = Self::default();
        for (_, entry) in db
            .iter_cf::<ChainEntry>(COL_CHAIN_ENTRY, IterMode::Start)
            .unwrap()
        {
            cache.hash.insert(entry.hash, entry);
        }
        for (height, hash) in db
            .iter_cf::<BlockHash>(COL_CHAIN_ENTRY_HASH, IterMode::Start)
            .unwrap()
        {
            let height = deserialize(&height).unwrap();
            cache.height.insert(height, hash);
        }
        cache
    }

    fn insert_height_batch(&mut self, entry: ChainEntry) {
        self.batch
            .push(ChainEntryCacheOperation::InsertHeight(entry));
    }

    fn insert_hash_batch(&mut self, entry: ChainEntry) {
        self.batch.push(ChainEntryCacheOperation::InsertHash(entry));
    }

    // fn remove_height_batch(&mut self, entry: &ChainEntry) {
    //     self.batch
    //         .push(ChainEntryCacheOperation::RemoveHeight(entry.height));
    // }

    // fn remove_hash_batch(&mut self, entry: &ChainEntry) {
    //     self.batch
    //         .push(ChainEntryCacheOperation::RemoveHash(entry.hash));
    // }

    fn write_batch(&mut self) {
        use ChainEntryCacheOperation::*;
        for operation in self.batch.drain(..) {
            match operation {
                InsertHash(entry) => {
                    self.hash.insert(entry.hash, entry);
                }
                InsertHeight(entry) => {
                    self.height.insert(entry.height, entry.hash);
                }
                // RemoveHeight(height) => self.height.remove(&height),
                // RemoveHash(hash) => self.hash.remove(&hash),
            };
        }
    }

    fn clear_batch(&mut self) {
        self.batch.clear();
    }
}

pub struct ChainDB {
    entry_cache: ChainEntryCache,
    pub state: ChainState,
    pub db: DiskDatabase,
    /// Allows for atomic writes to the database
    current: Option<Batch>,
    /// Allows for atomic chain state update
    pending: Option<ChainState>,
    network_params: NetworkParams,
    invalid: HashSet<BlockHash>,
    pub most_work: Option<ChainEntry>,
    pub blocks: BlockStore,
    pub version_bits_cache: VersionBitsCache,
}

pub struct ChainDBOptions {
    pub path: std::path::PathBuf,
}

impl ChainDB {
    pub fn new(network_params: NetworkParams, options: ChainDBOptions) -> Self {
        let db = DiskDatabase::new(options.path.clone());
        let entry_cache = ChainEntryCache::new(&db);

        Self {
            blocks: BlockStore::new(BlockStoreOptions::new(network_params.network, {
                let mut path = options.path;
                path.push("blocks");
                path
            })),
            version_bits_cache: VersionBitsCache::new(&network_params),
            network_params,
            db,
            entry_cache,
            state: Default::default(),
            current: Default::default(),
            pending: Default::default(),
            invalid: Default::default(),
            most_work: Default::default(),
        }
    }

    pub fn open(&mut self) -> Result<(), DBError> {
        match self.get_state()? {
            Some(state) => {
                self.version_bits_cache = self.get_version_bits_cache()?;

                self.state = state;
                info!("ChainDB successfully loaded.");
            }
            None => {
                self.save_genesis()?;
                info!("ChainDB successfully initialized.");
            }
        }

        self.set_most_work()?;

        info!(
            "Chain State: hash={} tx={} coin={} value={}.",
            self.state.tip,
            self.state.tx,
            self.state.coin,
            Amount::from_sat(self.state.value)
        );

        let most_work = self.most_work.as_ref().unwrap();

        info!(
            "Most work header: hash={} height={}",
            most_work.hash, most_work.height
        );

        Ok(())
    }

    fn get_version_bits_cache(&self) -> Result<VersionBitsCache, DBError> {
        let mut cache = VersionBitsCache::new(&self.network_params);

        let items = self
            .db
            .iter_cf::<ThresholdState>(COL_VERSION_BIT_STATE, IterMode::Start)?;

        for (key, state) in items {
            let mut decoder = std::io::Cursor::new(key);
            let bit = u8::consensus_decode(&mut decoder)?;
            let hash = BlockHash::consensus_decode(&mut decoder)?;
            cache.insert(&bit, hash, state);
        }

        Ok(cache)
    }

    fn save_version_bits_cache(&mut self) -> Result<(), DBError> {
        let updates = &self.version_bits_cache.updates;

        if updates.is_empty() {
            return Ok(());
        }

        info!("Saving {} state cache updates.", updates.len());

        assert!(self.current.is_some());

        for update in updates {
            self.current
                .as_mut()
                .unwrap()
                .insert(Key::VersionBitState(update.bit, update.hash), &update.state)?;
        }

        Ok(())
    }

    pub fn write_block(&mut self, block: &Block) -> Result<(), DBError> {
        let hash = block.block_hash();
        self.blocks.write(hash, &serialize(block))
    }

    pub fn get_block(&self, hash: BlockHash) -> Result<Option<Block>, DBError> {
        let raw = self.get_raw_block(hash)?;
        if let Some(raw) = raw {
            let block = deserialize(&raw)?;
            Ok(Some(block))
        } else {
            Ok(None)
        }
    }

    pub fn get_raw_block(&self, hash: BlockHash) -> Result<Option<Vec<u8>>, DBError> {
        self.blocks.read(hash, 0, None)
    }

    fn set_most_work(&mut self) -> Result<(), DBError> {
        self.most_work = self.get_most_work_entry()?;
        assert!(self.most_work.is_some());
        Ok(())
    }

    fn get_most_work_entry(&self) -> Result<Option<ChainEntry>, DBError> {
        Ok(self
            .get_tip_entries()?
            .next()
            .and_then(|(_, entry)| Some(entry)))
    }

    fn get_tip_entries(&self) -> Result<Iter<ChainEntry>, DBError> {
        self.db.iter_cf(COL_CHAIN_WORK, IterMode::End)
    }

    fn get_state(&self) -> Result<Option<ChainState>, DBError> {
        self.db.get(Key::ChainState)
    }

    fn save_genesis(&mut self) -> Result<(), DBError> {
        use bitcoin::blockdata::constants::genesis_block;
        let genesis = genesis_block(self.network_params.network);
        let entry = ChainEntry::from_block(&genesis, None);

        info!("Writing genesis block to ChainDB.");

        // Save the header entry.
        self.save_entry(entry, &ChainEntry::default())?;

        self.write_block(&genesis)?;

        // Connect to the main chain.
        self.connect(entry, &genesis, &CoinView::default())?;

        Ok(())
    }

    pub fn connect(
        &mut self,
        entry: ChainEntry,
        block: &Block,
        view: &CoinView,
    ) -> Result<(), DBError> {
        self.start();
        let mut connect = || {
            let hash = block.block_hash();

            // Hash -> Next Block
            if !entry.is_genesis() {
                self.batch()
                    .insert(Key::ChainNextHash(entry.prev_block), &hash)?;
            }

            // Height -> Hash
            self.batch()
                .insert(Key::ChainEntryHash(entry.height), &hash)?;
            self.entry_cache.insert_height_batch(entry);

            // Hash -> Entry
            self.entry_cache.insert_hash_batch(entry);

            // update version bits cache
            self.save_version_bits_cache()?;

            // Connect block and save coins.
            self.connect_block(entry, block, view)?;

            let chain_state = *self.pending.as_mut().unwrap().commit(hash);
            self.batch().insert(Key::ChainState, &chain_state)?;

            Ok(())
        };
        if let Err(err) = connect() {
            self.drop();
            return Err(err);
        }
        self.commit()?;
        Ok(())
    }

    fn batch(&mut self) -> &mut Batch {
        assert!(self.current.is_some());
        self.current.as_mut().unwrap()
    }

    fn commit(&mut self) -> Result<(), DBError> {
        assert!(self.current.is_some());
        assert!(self.pending.is_some());

        let current = self.current.take().unwrap();
        let pending = self.pending.take().unwrap();

        if let Err(error) = self.db.write_batch(current) {
            self.entry_cache.clear_batch();
            return Err(error);
        }

        if pending.commited {
            self.state = pending;
        }

        self.entry_cache.write_batch();
        self.version_bits_cache.commit();

        Ok(())
    }

    fn drop(&mut self) {
        assert!(self.current.is_some());
        assert!(self.pending.is_some());

        self.current.take();
        self.pending.take();

        self.entry_cache.clear_batch();
        self.version_bits_cache.drop();
    }

    pub fn get_tip(&self) -> Option<&ChainEntry> {
        self.get_entry_by_hash(&self.state.tip)
    }

    pub fn get_entry_by_hash(&self, hash: &BlockHash) -> Option<&ChainEntry> {
        self.entry_cache.hash.get(hash)
    }

    pub fn get_entry_by_height(&self, height: &u32) -> Option<&ChainEntry> {
        let hash = self.entry_cache.height.get(height)?;
        Some(&self.entry_cache.hash[hash])
    }

    pub fn get_next_path<'a>(
        &'a self,
        mut entry: &'a ChainEntry,
        height: u32,
        limit: usize,
    ) -> VecDeque<&ChainEntry> {
        let mut entries = VecDeque::new();

        if limit == 0 {
            return entries;
        }

        let start = height + limit as u32;

        if start < entry.height {
            entry = self.get_ancestor(&entry, start);
        }

        while entry.height > height {
            entries.push_front(entry);
            entry = self.get_entry_by_hash(&entry.prev_block).unwrap();
        }

        entries
    }

    pub fn has_block(&self, hash: BlockHash) -> Result<bool, DBError> {
        self.blocks.has(hash)
    }

    pub fn read_coin(&self, prevout: OutPoint) -> Result<Option<CoinEntry>, DBError> {
        self.db.get(Key::Coin(prevout))
    }

    pub fn has_coins(&self, tx: &Transaction) -> bool {
        let txid = tx.txid();

        (0..tx.output.len() as u32).any(|vout| {
            let key = Key::Coin(OutPoint { txid, vout });
            self.db.has(key).unwrap()
        })
    }

    fn start(&mut self) {
        assert!(self.current.is_none());
        assert!(self.pending.is_none());

        self.current.replace(Batch::default());
        self.pending.replace(self.state);

        self.entry_cache.clear_batch();
    }

    fn connect_block(
        &mut self,
        entry: ChainEntry,
        block: &Block,
        view: &CoinView,
    ) -> Result<(), DBError> {
        let pending = self.pending.as_mut().unwrap();
        pending.connect(block);

        if entry.is_genesis() {
            return Ok(());
        }

        for (i, tx) in block.txdata.iter().enumerate() {
            if i > 0 {
                for input in &tx.input {
                    let spent = view
                        .get_output(&input.previous_output)
                        .expect("already checked before connect");
                    pending.spend(spent);
                }
            }

            for output in &tx.output {
                if output.script_pubkey.is_provably_unspendable() {
                    continue;
                }
                pending.add(output);
            }
        }

        self.save_view(view)?;

        Ok(())
    }

    fn save_view(&mut self, view: &CoinView) -> Result<(), DBError> {
        for (outpoint, coin) in &view.map {
            if coin.spent {
                self.batch().remove(Key::Coin(*outpoint));
                continue;
            }
            self.batch().insert(Key::Coin(*outpoint), coin)?;
        }
        Ok(())
    }

    fn get_skip<'a>(&'a self, entry: &'a ChainEntry) -> &ChainEntry {
        self.get_ancestor(entry, get_skip_height(entry.height))
    }

    pub fn get_ancestor<'a>(&'a self, entry: &'a ChainEntry, height: u32) -> &ChainEntry {
        assert!(height <= entry.height);

        if self.is_main_chain(entry) {
            return self
                .get_entry_by_height(&height)
                .expect("in main chain so must exist");
        }

        let mut entry = entry;

        while entry.height != height {
            let skip = get_skip_height(entry.height) as i32;
            let prev = get_skip_height(entry.height + 1) as i32;

            let skip_better = skip > height as i32;
            let prev_better = prev < skip - 2 && prev >= height as i32;

            let hash = match entry.skip {
                skip_hash if skip_hash != Default::default() => {
                    if skip == height as i32 || (skip_better && !prev_better) {
                        skip_hash
                    } else {
                        entry.prev_block
                    }
                }
                _ => entry.prev_block,
            };

            entry = self
                .get_entry_by_hash(&hash)
                .expect("ancestor should always exist");
        }

        entry
    }

    pub fn is_main_hash(&self, hash: &BlockHash) -> bool {
        match self.get_entry_by_hash(hash) {
            None => false,
            Some(entry) => self.is_main_chain(entry),
        }
    }

    pub fn is_main_chain(&self, entry: &ChainEntry) -> bool {
        match self.entry_cache.height.get(&entry.height) {
            Some(main_entry_hash) => *main_entry_hash == entry.hash,
            None => false,
        }
    }

    pub fn get_next_hashes(&self, hash: BlockHash) -> Result<Vec<BlockHash>, DBError> {
        let hashes = self.db.get(Key::ChainNextHashes(hash))?.unwrap_or_default();
        Ok(hashes)
    }

    pub fn get_next_entries(&self, hash: BlockHash) -> Result<Vec<ChainEntry>, DBError> {
        let hashes = self.get_next_hashes(hash)?;
        let mut entries = Vec::with_capacity(hashes.len());
        for hash in hashes {
            entries.push(*self.get_entry_by_hash(&hash).expect("database corruption"));
        }
        Ok(entries)
    }

    pub fn get_next_hash(&self, hash: BlockHash) -> Result<Option<BlockHash>, DBError> {
        self.db.get(Key::ChainNextHash(hash))
    }

    pub fn get_next(&self, entry: &ChainEntry) -> Result<Option<&ChainEntry>, DBError> {
        if let Some(hash) = self.get_next_hash(entry.hash)? {
            Ok(self.get_entry_by_hash(&hash))
        } else {
            Ok(None)
        }
    }

    // TODO: store on disk as well
    pub fn has_invalid(&self, hash: &BlockHash) -> bool {
        self.invalid.contains(hash)
    }

    pub fn set_invalid(&mut self, hash: BlockHash) {
        self.invalid.insert(hash);
    }

    pub fn has_header(&self, hash: &BlockHash) -> bool {
        self.entry_cache.hash.contains_key(hash)
    }

    pub fn save_entry(&mut self, mut entry: ChainEntry, prev: &ChainEntry) -> Result<(), DBError> {
        self.start();
        let mut save_entry = || {
            let hash = entry.hash;

            // Hash -> Height
            self.batch()
                .insert(Key::ChainEntryHeight(hash), &entry.height)?;

            // Hash -> Entry
            self.batch().insert(Key::ChainEntry(hash), &entry)?;
            self.entry_cache.insert_hash_batch(entry);

            // Tip chainwork index.
            self.batch()
                .remove(Key::ChainWork(prev.chainwork, prev.hash));
            self.batch()
                .insert(Key::ChainWork(entry.chainwork, hash), &entry)?;

            if !entry.is_genesis() {
                let skip = self.get_skip(&entry);
                entry.skip = skip.hash;
            }

            let mut nexts = self.get_next_hashes(prev.hash)?;
            nexts.push(hash);
            self.batch()
                .insert(Key::ChainNextHashes(prev.hash), &nexts)?;

            self.save_version_bits_cache()?;

            Ok(())
        };
        if let Err(err) = save_entry() {
            self.drop();
            return Err(err);
        }
        self.commit()?;
        self.update_most_work(entry);
        Ok(())
    }

    fn update_most_work(&mut self, entry: ChainEntry) {
        match self.most_work {
            None => self.most_work = Some(entry),
            Some(most_work) if entry.chainwork > most_work.chainwork => {
                self.most_work = Some(entry);
            }
            Some(most_work)
                if entry.chainwork == most_work.chainwork && entry.hash > most_work.hash =>
            {
                self.most_work = Some(entry);
            }
            _ => (), // current most work entry is better than given entry
        }
    }
}

pub fn get_skip_height(height: u32) -> u32 {
    if height < 2 {
        return 0;
    }

    let flip_low = |n| n & (n - 1);

    if height & 1 == 1 {
        flip_low(flip_low(height - 1) + 1)
    } else {
        flip_low(height)
    }
}

/// Current state of the main chain
#[derive(Default, Clone, Debug, Copy)]
pub struct ChainState {
    /// Hash of the tip of the chain
    pub tip: BlockHash,
    /// Number of transactions that have occurred
    pub tx: u64,
    /// Nnumber of unspent coins
    pub coin: u64,
    /// The value of unspent coins
    pub value: u64,
    pub commited: bool,
}

impl ChainState {
    pub fn connect(&mut self, block: &Block) {
        self.tx += block.txdata.len() as u64;
    }

    pub fn add(&mut self, coin: &TxOut) {
        self.coin += 1;
        self.value += coin.value;
    }

    pub fn spend(&mut self, coin: &TxOut) {
        self.coin -= 1;
        self.value -= coin.value;
    }

    pub fn commit(&mut self, hash: BlockHash) -> &ChainState {
        self.tip = hash;
        self.commited = true;
        self
    }
}

#[derive(Default)]
pub struct VersionBitsCache {
    bits: HashMap<u8, HashMap<BlockHash, ThresholdState>>,
    updates: Vec<VersionBitsCacheUpdate>,
}

impl VersionBitsCache {
    pub fn new(network: &NetworkParams) -> VersionBitsCache {
        let mut cache = VersionBitsCache::default();
        for (_, deployment) in network.deployments.iter() {
            assert!(!cache.bits.contains_key(&deployment.bit));
            cache.bits.insert(deployment.bit, HashMap::new());
        }
        cache
    }

    pub fn set(&mut self, bit: u8, hash: BlockHash, state: ThresholdState) {
        let cache = self.cache_mut(&bit);
        if let Some(cached_state) = cache.get(&hash) {
            if cached_state != &state {
                cache.insert(hash, state);
                self.updates
                    .push(VersionBitsCacheUpdate::new(bit, hash, state));
            }
        }
    }

    pub fn get(&self, bit: u8, hash: &BlockHash) -> Option<&ThresholdState> {
        self.bits
            .get(&bit)
            .expect("unknown deployment bit")
            .get(hash)
    }

    pub fn commit(&mut self) {
        self.updates.clear();
    }

    pub fn drop(&mut self) {
        for update in self.updates.drain(..) {
            self.bits
                .get_mut(&update.bit)
                .expect("unknown deployment bit")
                .remove(&update.hash);
        }
    }

    fn cache_mut(&mut self, bit: &u8) -> &mut HashMap<BlockHash, ThresholdState> {
        self.bits.get_mut(bit).expect("unknown deployment bit")
    }

    pub fn insert(&mut self, bit: &u8, hash: BlockHash, state: ThresholdState) {
        self.cache_mut(bit).insert(hash, state);
    }
}

pub struct VersionBitsCacheUpdate {
    bit: u8,
    hash: BlockHash,
    state: ThresholdState,
}

impl VersionBitsCacheUpdate {
    pub fn new(bit: u8, hash: BlockHash, state: ThresholdState) -> Self {
        Self { bit, hash, state }
    }
}
