use super::ChainEntry;
use crate::blockstore::{BlockStore, BlockStoreOptions};
use crate::coins::{CoinEntry, CoinView};
use crate::db::{
    Batch, Database, DiskDatabase, Iter, IterMode, Key, COL_CHAIN_WORK, COL_VERSION_BIT_STATE,
};
use crate::protocol::{NetworkParams, ThresholdState, VERSIONBITS_NUM_BITS};
use crate::util::EmptyResult;
use bitcoin::Amount;
use bitcoin::{
    consensus::encode::{deserialize, serialize},
    Block, BlockHash, OutPoint, TxOut,
};
use failure::Error;
use log::info;
use std::collections::{hash_map::Entry, HashMap, HashSet, VecDeque};

#[derive(Default)]
pub struct ChainEntryCache {
    height: HashMap<u32, ChainEntry>,
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
                InsertHash(entry) => self.hash.insert(entry.hash, entry),
                InsertHeight(entry) => self.height.insert(entry.height, entry),
                // RemoveHeight(height) => self.height.remove(&height),
                // RemoveHash(hash) => self.hash.remove(&hash),
            };
        }
    }

    fn clear_batch(&mut self) {
        self.batch.clear();
    }
}

enum CoinCacheOperation {
    Insert(OutPoint, CoinEntry),
    Remove(OutPoint),
}

#[derive(Default)]
pub struct CoinCache {
    coins: HashMap<OutPoint, CoinEntry>,
    batch: Vec<CoinCacheOperation>,
}

impl CoinCache {
    fn insert_batch(&mut self, outpoint: OutPoint, coin_entry: CoinEntry) {
        self.batch
            .push(CoinCacheOperation::Insert(outpoint, coin_entry));
    }

    fn remove_batch(&mut self, outpoint: OutPoint) {
        self.batch.push(CoinCacheOperation::Remove(outpoint));
    }

    fn write_batch(&mut self) {
        use CoinCacheOperation::*;
        for operation in self.batch.drain(..) {
            match operation {
                Insert(outpoint, entry) => {
                    self.coins.insert(outpoint, entry);
                }
                Remove(outpoint) => {
                    self.coins.remove(&outpoint);
                }
            }
        }
    }

    fn clear_batch(&mut self) {
        self.batch.clear();
    }
}

pub struct ChainDB {
    pub coin_cache: CoinCache,
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
        Self {
            blocks: BlockStore::new(BlockStoreOptions::new(
                network_params.network,
                "/home/jrawsthorne/Documents/Projects/rust/rust_bitcoin_node/data/blocks"
                    .to_string(),
            )),
            version_bits_cache: VersionBitsCache::new(&network_params),
            network_params,
            db: DiskDatabase::new(options.path),
            coin_cache: Default::default(),
            entry_cache: Default::default(),
            state: Default::default(),
            current: Default::default(),
            pending: Default::default(),
            invalid: Default::default(),
            most_work: Default::default(),
        }
    }

    pub async fn open(&mut self) -> EmptyResult {
        match self.get_state()? {
            Some(state) => {
                self.version_bits_cache = self.get_version_bits_cache()?;

                self.state = state;
                info!("ChainDB successfully loaded.");
            }
            None => {
                self.save_genesis().await?;
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

        Ok(())
    }

    fn get_version_bits_cache(&self) -> Result<VersionBitsCache, Error> {
        use bitcoin::consensus::Decodable;

        let mut cache = VersionBitsCache::new(&self.network_params);

        let items = self
            .db
            .iter_cf::<ThresholdState>(COL_VERSION_BIT_STATE, IterMode::Start)?;

        for (key, state) in items {
            let mut decoder = std::io::Cursor::new(key);
            let bit = u8::consensus_decode(&mut decoder)?;
            let hash = BlockHash::consensus_decode(&mut decoder)?;
            cache.insert(bit, hash, state);
        }

        Ok(cache)
    }

    fn save_version_bits_cache(&mut self) -> EmptyResult {
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

    pub async fn write_block(&mut self, block: &Block) -> EmptyResult {
        let hash = block.block_hash();
        self.blocks.write(hash, &serialize(block)).await
    }

    pub async fn get_block(&self, hash: BlockHash) -> Result<Option<Block>, Error> {
        let raw = self.get_raw_block(hash).await?;
        if let Some(raw) = raw {
            let block = deserialize(&raw)?;
            Ok(Some(block))
        } else {
            Ok(None)
        }
    }

    pub async fn get_raw_block(&self, hash: BlockHash) -> Result<Option<Vec<u8>>, Error> {
        self.blocks.read(hash, 0, None).await
    }

    fn set_most_work(&mut self) -> EmptyResult {
        self.most_work = self.get_most_work_entry()?;
        assert!(self.most_work.is_some());
        Ok(())
    }

    fn get_most_work_entry(&self) -> Result<Option<ChainEntry>, Error> {
        Ok(self
            .get_tip_entries()?
            .next()
            .and_then(|(_, entry)| Some(entry)))
    }

    fn get_tip_entries(&self) -> Result<Iter<ChainEntry>, Error> {
        self.db.iter_cf(COL_CHAIN_WORK, IterMode::End)
    }

    fn get_state(&self) -> Result<Option<ChainState>, Error> {
        self.db.get(Key::ChainState)
    }

    async fn save_genesis(&mut self) -> EmptyResult {
        use bitcoin::blockdata::constants::genesis_block;
        let genesis = genesis_block(self.network_params.network);
        let entry = ChainEntry::from_block(&genesis, None);

        info!("Writing genesis block to ChainDB.");

        // Save the header entry.
        self.save_entry(entry, &ChainEntry::default())?;

        self.write_block(&genesis).await?;

        // Connect to the main chain.
        self.connect(entry, &genesis, CoinView::default())?;

        Ok(())
    }

    pub fn connect(&mut self, entry: ChainEntry, block: &Block, view: CoinView) -> EmptyResult {
        self.start();
        let connect = || {
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

    fn commit(&mut self) -> EmptyResult {
        assert!(self.current.is_some());
        assert!(self.pending.is_some());

        let current = self.current.take().unwrap();
        let pending = self.pending.take().unwrap();

        if let Err(error) = self.db.write_batch(current) {
            self.entry_cache.clear_batch();
            self.coin_cache.clear_batch();
            return Err(error);
        }

        if pending.commited {
            self.state = pending;
        }

        self.entry_cache.write_batch();
        self.coin_cache.write_batch();
        self.version_bits_cache.commit();

        Ok(())
    }

    fn drop(&mut self) {
        assert!(self.current.is_some());
        assert!(self.pending.is_some());

        self.current.take();
        self.pending.take();

        self.entry_cache.clear_batch();
        self.coin_cache.clear_batch();
        self.version_bits_cache.drop();
    }

    pub fn get_tip(&mut self) -> Result<Option<&ChainEntry>, Error> {
        let tip = self.state.tip;
        self.get_entry_by_hash(tip)
    }

    pub fn get_entry_by_hash(&mut self, hash: BlockHash) -> Result<Option<&ChainEntry>, Error> {
        Self::entry_by_hash(&mut self.entry_cache.hash, &self.db, hash)
    }

    pub fn entry_by_hash<'a>(
        cache: &'a mut HashMap<BlockHash, ChainEntry>,
        db: &'a DiskDatabase,
        hash: BlockHash,
    ) -> Result<Option<&'a ChainEntry>, Error> {
        Ok(match cache.entry(hash) {
            Entry::Occupied(entry) => Some(entry.into_mut()),
            Entry::Vacant(entry) => db
                .get(Key::ChainEntry(hash))?
                .and_then(|chain_entry| Some(&*entry.insert(chain_entry))),
        })
    }

    pub fn entry_by_height<'a>(
        height_cache: &'a mut HashMap<u32, ChainEntry>,
        hash_cache: &'a mut HashMap<BlockHash, ChainEntry>,
        db: &'a DiskDatabase,
        height: u32,
    ) -> Result<Option<&'a ChainEntry>, Error> {
        Ok(match height_cache.entry(height) {
            Entry::Occupied(entry) => Some(entry.into_mut()),
            Entry::Vacant(entry) => {
                if let Some(hash) = db.get(Key::ChainEntryHash(height))? {
                    if let Some(chain_entry) = Self::entry_by_hash(hash_cache, db, hash)? {
                        entry.insert(*chain_entry);
                        return Ok(Some(chain_entry));
                    }
                }
                None
            }
        })
    }

    pub fn get_entry_by_height(&mut self, height: u32) -> Result<Option<&ChainEntry>, Error> {
        Self::entry_by_height(
            &mut self.entry_cache.height,
            &mut self.entry_cache.hash,
            &self.db,
            height,
        )
    }

    pub fn get_next_path(
        &mut self,
        mut entry: ChainEntry,
        height: u32,
        limit: usize,
    ) -> Result<VecDeque<ChainEntry>, Error> {
        let mut entries = VecDeque::new();

        if limit == 0 {
            return Ok(entries);
        }

        let start = height + limit as u32;

        if start < entry.height {
            entry = self.get_ancestor(&entry, start)?;
        }

        while entry.height > height {
            entries.push_front(entry);
            entry = *self.get_entry_by_hash(entry.prev_block)?.unwrap();
        }

        Ok(entries)
    }

    pub async fn has_block(&self, hash: BlockHash) -> Result<bool, Error> {
        self.blocks.has(hash).await
    }

    pub fn read_coin(&mut self, prevout: OutPoint) -> Result<Option<&CoinEntry>, Error> {
        Ok(match self.coin_cache.coins.entry(prevout) {
            Entry::Occupied(entry) => Some(&*entry.into_mut()),
            Entry::Vacant(entry) => match self.db.get(Key::Coin(prevout))? {
                Some(coin) => Some(&*entry.insert(coin)),
                None => None,
            },
        })
    }

    fn start(&mut self) {
        assert!(self.current.is_none());
        assert!(self.pending.is_none());

        self.current.replace(Batch::default());
        self.pending.replace(self.state);

        self.entry_cache.clear_batch();
        self.coin_cache.clear_batch();
    }

    fn connect_block(&mut self, entry: ChainEntry, block: &Block, view: CoinView) -> EmptyResult {
        let pending = self.pending.as_mut().unwrap();
        pending.connect(block);

        if entry.is_genesis() {
            return Ok(());
        }

        for (i, tx) in block.txdata.iter().enumerate() {
            if i > 0 {
                for input in &tx.input {
                    pending.spend(
                        view.get_output(&input.previous_output)
                            .expect("already checked before connect"),
                    );
                }
            }

            for output in tx
                .output
                .iter()
                .filter(|output| !output.script_pubkey.is_provably_unspendable())
            {
                pending.add(output);
            }
        }

        self.save_view(view)?;

        Ok(())
    }

    fn save_view(&mut self, view: CoinView) -> EmptyResult {
        for (outpoint, coin) in view.map {
            if coin.spent {
                self.coin_cache.remove_batch(outpoint);
                self.batch().remove(Key::Coin(outpoint))?;
                continue;
            }
            self.batch().insert(Key::Coin(outpoint), &coin)?;
            self.coin_cache.insert_batch(outpoint, coin);
        }
        Ok(())
    }

    fn get_skip_height(height: u32) -> u32 {
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

    fn get_skip(&mut self, entry: &ChainEntry) -> Result<ChainEntry, Error> {
        self.get_ancestor(entry, Self::get_skip_height(entry.height))
    }

    pub fn get_ancestor(&mut self, entry: &ChainEntry, height: u32) -> Result<ChainEntry, Error> {
        assert!(height <= entry.height);

        if self.is_main_chain(entry)? {
            return Ok(*self
                .get_entry_by_height(height)?
                .expect("in main chain so must exist"));
        }

        let mut entry = *entry;

        while entry.height != height {
            let skip = Self::get_skip_height(entry.height) as i32;
            let prev = Self::get_skip_height(entry.height + 1) as i32;

            let skip_better = skip > height as i32;
            let prev_better = prev < skip - 2 && prev >= height as i32;

            let skip_hash = self.db.get(Key::ChainSkip(entry.hash))?;

            let hash = match skip_hash {
                Some(skip_hash) if (skip == height as i32 || (skip_better && !prev_better)) => {
                    skip_hash
                }
                _ => entry.prev_block,
            };

            entry = *self
                .get_entry_by_hash(hash)?
                .expect("ancestor should always exist");
        }

        Ok(entry)
    }

    pub fn is_main_hash(&mut self, hash: BlockHash) -> Result<bool, Error> {
        if hash == BlockHash::default() {
            return Ok(false);
        }

        if hash == self.state.tip {
            return Ok(true);
        }

        if let Some(cache) = self.get_entry_by_hash(hash)? {
            let cache_heigt = cache.height;
            let height = self.get_entry_by_height(cache_heigt)?;
            if let Some(height) = height {
                return Ok(height.hash == hash);
            }
        }

        if self.get_next_hash(hash)?.is_some() {
            return Ok(true);
        }

        Ok(false)
    }

    pub fn is_main_chain(&self, entry: &ChainEntry) -> Result<bool, Error> {
        if entry.is_genesis() {
            return Ok(true);
        }

        if entry.hash == self.state.tip {
            return Ok(true);
        }

        if let Some(cache) = self.entry_cache.height.get(&entry.height) {
            return Ok(cache.hash == entry.hash);
        }

        if self.get_next_hash(entry.hash)?.is_some() {
            return Ok(true);
        }

        Ok(false)
    }

    pub fn get_next_hashes(&self, hash: BlockHash) -> Result<Vec<BlockHash>, Error> {
        let hashes = self.db.get(Key::ChainNextHashes(hash))?.unwrap_or_default();
        Ok(hashes)
    }

    pub fn get_next_entries(&mut self, hash: BlockHash) -> Result<Vec<ChainEntry>, Error> {
        let hashes = self.get_next_hashes(hash)?;
        let mut entries = Vec::with_capacity(hashes.len());
        for hash in hashes {
            entries.push(*self.get_entry_by_hash(hash)?.expect("database corruption"));
        }
        Ok(entries)
    }

    pub fn get_next_hash(&self, hash: BlockHash) -> Result<Option<BlockHash>, Error> {
        self.db.get(Key::ChainNextHash(hash))
    }

    pub fn get_next(&mut self, entry: &ChainEntry) -> Result<Option<&ChainEntry>, Error> {
        if let Some(hash) = self.get_next_hash(entry.hash)? {
            self.get_entry_by_hash(hash)
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

    pub fn has_header(&self, hash: BlockHash) -> Result<bool, Error> {
        if self.entry_cache.hash.contains_key(&hash) {
            Ok(true)
        } else {
            self.db.has(Key::ChainEntry(hash))
        }
    }

    pub fn save_entry(&mut self, entry: ChainEntry, prev: &ChainEntry) -> EmptyResult {
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
                .remove(Key::ChainWork(prev.chainwork, prev.hash))?;
            self.batch()
                .insert(Key::ChainWork(entry.chainwork, hash), &entry)?;

            if !entry.is_genesis() {
                let skip = self.get_skip(&entry)?;
                self.batch().insert(Key::ChainSkip(hash), &skip.hash)?;
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

pub struct VersionBitsCache {
    bits: Vec<Option<HashMap<BlockHash, ThresholdState>>>,
    updates: Vec<VersionBitsCacheUpdate>,
}

impl VersionBitsCache {
    pub fn new(network: &NetworkParams) -> VersionBitsCache {
        let mut cache = VersionBitsCache {
            bits: vec![None; VERSIONBITS_NUM_BITS],
            updates: vec![],
        };
        for (_, deployment) in network.deployments.iter() {
            assert!(cache.bits[deployment.bit as usize].is_none());
            cache.bits[deployment.bit as usize] = Some(HashMap::new());
        }
        cache
    }

    pub fn set(&mut self, bit: u8, hash: BlockHash, state: ThresholdState) {
        let cache = match self.bits.get_mut(bit as usize) {
            Some(Some(cache)) => cache,
            _ => panic!("unknown deployment bit"),
        };

        if match cache.get(&hash) {
            None => true,
            Some(cache_state) if cache_state != &state => true,
            _ => false,
        } {
            cache.insert(hash, state);
            self.updates
                .push(VersionBitsCacheUpdate::new(bit, hash, state));
        }
    }

    pub fn get(&self, bit: u8, hash: &BlockHash) -> Option<ThresholdState> {
        let cache = match self.bits.get(bit as usize) {
            Some(Some(cache)) => cache,
            _ => panic!("unknown deployment bit"),
        };

        cache.get(hash).copied()
    }

    pub fn commit(&mut self) {
        self.updates.clear();
    }

    pub fn drop(&mut self) {
        for update in self.updates.drain(..) {
            let cache = match self.bits.get_mut(update.bit as usize) {
                Some(Some(cache)) => cache,
                _ => panic!("unknown deployment bit"),
            };
            cache.remove(&update.hash);
        }
    }

    pub fn insert(&mut self, bit: u8, hash: BlockHash, state: ThresholdState) {
        let cache = match self.bits.get_mut(bit as usize) {
            Some(Some(cache)) => cache,
            _ => panic!("unknown deployment bit"),
        };
        cache.insert(hash, state);
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
