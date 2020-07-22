use super::ChainEntry;
use crate::blockstore::{BlockStore, BlockStoreOptions};
use crate::coins::{CoinEntry, CoinView};
use crate::db::{Batch, DBKey, Database, DiskDatabase, Iter, IterMode};
use crate::error::DBError;
use crate::protocol::{NetworkParams, ThresholdState};
use bitcoin::Amount;
use bitcoin::{
    consensus::{encode::deserialize, Decodable, Encodable},
    util::uint::Uint256,
    Block, BlockHash, OutPoint, Transaction, TxOut,
};
use log::info;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

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
    pub fn new(db: &DiskDatabase<Key>) -> Self {
        info!("Populating entry cache");
        let mut cache = Self::default();
        for (_, entry) in db
            .iter_cf::<ChainEntry>(COL_ENTRY, IterMode::Start)
            .unwrap()
        {
            cache.hash.insert(entry.hash, entry);
        }
        for (height, hash) in db.iter_cf::<BlockHash>(COL_HASH, IterMode::Start).unwrap() {
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
    pub db: DiskDatabase<Key>,
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
        let db = DiskDatabase::new(options.path.clone(), key::columns());
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

    fn save_version_bits_cache(&mut self, batch: &mut Batch<Key>) -> Result<(), DBError> {
        let updates = &self.version_bits_cache.updates;

        if updates.is_empty() {
            return Ok(());
        }

        info!("Saving {} state cache updates.", updates.len());

        for update in updates {
            batch.insert(Key::VersionBitState(update.bit, update.hash), &update.state)?;
        }

        Ok(())
    }

    pub fn write_block(&mut self, block: &Block) -> Result<(), DBError> {
        let hash = block.block_hash();
        let mut b = Vec::with_capacity(block.get_size());
        block.consensus_encode(&mut b).unwrap();
        self.blocks.write(hash, &b)
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
        let mut batch = self.start();
        let mut connect = || {
            let hash = block.block_hash();

            // Hash -> Next Block
            if !entry.is_genesis() {
                batch.insert(Key::NextHash(entry.prev_block), &hash)?;
            }

            // Height -> Hash
            batch.insert(Key::Hash(entry.height), &hash)?;
            self.entry_cache.insert_height_batch(entry);

            // Hash -> Entry
            self.entry_cache.insert_hash_batch(entry);

            // update version bits cache
            self.save_version_bits_cache(&mut batch)?;

            // Connect block and save coins.
            self.connect_block(&mut batch, entry, block, view)?;

            let chain_state = self.pending.as_mut().unwrap().commit(hash);
            batch.insert(Key::ChainState, &chain_state)?;

            Ok(())
        };
        if let Err(err) = connect() {
            self.drop();
            return Err(err);
        }
        self.commit(batch)?;
        Ok(())
    }

    pub fn disconnect(&mut self) {}

    fn commit(&mut self, batch: Batch<Key>) -> Result<(), DBError> {
        assert!(self.pending.is_some());

        let pending = self.pending.take().unwrap();

        if let Err(error) = self.db.write_batch(batch) {
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
        assert!(self.pending.is_some());

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

    pub fn get_entry_by_height(&self, height: u32) -> Option<&ChainEntry> {
        let hash = self.entry_cache.height.get(&height)?;
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

    pub fn has_coins(&self, tx: &Transaction) -> Result<bool, DBError> {
        let txid = tx.txid();

        for vout in 0..tx.output.len() {
            let key = Key::Coin(OutPoint {
                txid,
                vout: vout as u32,
            });
            if self.db.has(key)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn start(&mut self) -> Batch<Key> {
        assert!(self.pending.is_none());

        let batch = Batch::new();
        self.pending.replace(self.state);

        self.entry_cache.clear_batch();

        batch
    }

    fn connect_block(
        &mut self,
        batch: &mut Batch<Key>,
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

        self.save_view(batch, view)?;

        Ok(())
    }

    fn save_view(&mut self, batch: &mut Batch<Key>, view: &CoinView) -> Result<(), DBError> {
        for (outpoint, coin) in &view.map {
            if coin.spent {
                batch.remove(Key::Coin(*outpoint));
                continue;
            }
            batch.insert(Key::Coin(*outpoint), coin)?;
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
                .get_entry_by_height(height)
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
        let hashes = self.db.get(Key::NextHashes(hash))?.unwrap_or_default();
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
        self.db.get(Key::NextHash(hash))
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
        let mut batch = self.start();
        let mut save_entry = || {
            let hash = entry.hash;

            // Hash -> Height
            batch.insert(Key::Height(hash), &entry.height)?;

            // Hash -> Entry
            batch.insert(Key::Entry(hash), &entry)?;
            self.entry_cache.insert_hash_batch(entry);

            // Tip chainwork index.
            batch.remove(Key::ChainWork(prev.chainwork, prev.hash));
            batch.insert(Key::ChainWork(entry.chainwork, hash), &entry)?;

            if !entry.is_genesis() {
                let skip = self.get_skip(&entry);
                entry.skip = skip.hash;
            }

            let mut nexts = self.get_next_hashes(prev.hash)?;
            nexts.push(hash);
            batch.insert(Key::NextHashes(prev.hash), &nexts)?;

            self.save_version_bits_cache(&mut batch)?;

            Ok(())
        };
        if let Err(err) = save_entry() {
            self.drop();
            return Err(err);
        }
        self.commit(batch)?;
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

    pub fn commit(mut self, hash: BlockHash) -> ChainState {
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

use key::*;

mod key {
    use super::*;
    use crate::db::{DBValue, KEY_CHAIN_STATE, KEY_TIP};
    use bitcoin::consensus::{encode, Encodable};

    pub const COL_ENTRY: &str = "E";
    pub const COL_HEIGHT: &str = "H";
    pub const COL_HASH: &str = "h";
    pub const COL_COIN: &str = "C";
    pub const COL_CHAIN_WORK: &str = "c";
    pub const COL_NEXT_HASH: &str = "N";
    pub const COL_NEXT_HASHES: &str = "n";
    pub const COL_MISC: &str = "M";
    pub const COL_SKIP: &str = "S";
    pub const COL_VERSION_BIT_STATE: &str = "V";

    pub fn columns() -> Vec<&'static str> {
        vec![
            COL_ENTRY,
            COL_HEIGHT,
            COL_HASH,
            COL_COIN,
            COL_CHAIN_WORK,
            COL_NEXT_HASH,
            COL_NEXT_HASHES,
            COL_MISC,
            COL_SKIP,
            COL_VERSION_BIT_STATE,
        ]
    }

    pub enum Key {
        Entry(BlockHash),
        Coin(OutPoint),
        Hash(u32),
        Height(BlockHash),
        NextHash(BlockHash),
        NextHashes(BlockHash),
        ChainState,
        Tip,
        ChainWork(Uint256, BlockHash),
        Skip(BlockHash),
        VersionBitState(u8, BlockHash),
    }

    impl Key {
        pub fn insert<V: DBValue>(&self, db: &rocksdb::DB, value: V) {
            match self {
                Key::Entry(hash)
                | Key::Height(hash)
                | Key::NextHash(hash)
                | Key::NextHashes(hash)
                | Key::Skip(hash) => self.i(db, hash, value),
                Key::Coin(outpoint) => {
                    let mut key = [0u8; 36];
                    outpoint.consensus_encode(&mut key[..]).unwrap();
                    self.i(db, key.as_ref(), value);
                }
                Key::Hash(a) => {
                    self.i(db, a.to_le_bytes(), value);
                }
                Key::ChainState => {
                    self.i(db, KEY_CHAIN_STATE, value);
                }
                Key::Tip => {
                    self.i(db, KEY_TIP, value);
                }
                Key::ChainWork(work, hash) => {
                    let mut key = [0u8; 64];
                    work.consensus_encode(&mut key[..]).unwrap();
                    hash.consensus_encode(&mut key[..]).unwrap();
                    self.i(db, key.as_ref(), value);
                }
                Key::VersionBitState(bit, hash) => {
                    let mut key = [0u8; 33];
                    bit.consensus_encode(&mut key[..]).unwrap();
                    hash.consensus_encode(&mut key[..]).unwrap();
                    self.i(db, key.as_ref(), value);
                }
            }
        }

        fn i(&self, db: &rocksdb::DB, key: impl AsRef<[u8]>, value: impl DBValue) {
            db.put_cf(
                db.cf_handle(self.col()).unwrap(),
                key,
                value.encode().unwrap(),
            )
            .unwrap()
        }
    }

    impl DBKey for Key {
        fn encode(&self) -> Result<Vec<u8>, encode::Error> {
            Ok(match self {
                Key::Coin(outpoint) => {
                    let mut encoder = Vec::with_capacity(36);
                    outpoint.txid.consensus_encode(&mut encoder)?;
                    outpoint.vout.consensus_encode(&mut encoder)?;
                    encoder
                }
                Key::Hash(height) => height.to_le_bytes().into(),
                Key::Entry(hash)
                | Key::Height(hash)
                | Key::NextHash(hash)
                | Key::NextHashes(hash)
                | Key::Skip(hash) => hash.as_ref().into(),
                Key::Tip => KEY_TIP.to_vec(),
                Key::ChainState => KEY_CHAIN_STATE.to_vec(),
                Key::ChainWork(work, hash) => {
                    let mut encoder = Vec::with_capacity(64);
                    work.consensus_encode(&mut encoder)?;
                    hash.consensus_encode(&mut encoder)?;
                    encoder
                }

                Key::VersionBitState(bit, hash) => {
                    let mut encoder = Vec::with_capacity(33);
                    bit.consensus_encode(&mut encoder)?;
                    hash.consensus_encode(&mut encoder)?;
                    encoder
                }
            })
        }

        fn col(&self) -> &'static str {
            match self {
                Key::Entry(_) => COL_ENTRY,
                Key::Coin(_) => COL_COIN,
                Key::Hash(_) => COL_HASH,
                Key::Height(_) => COL_HEIGHT,
                Key::NextHash(_) => COL_NEXT_HASH,
                Key::ChainState | Key::Tip => COL_MISC,
                Key::ChainWork(_, _) => COL_CHAIN_WORK,
                Key::NextHashes(_) => COL_NEXT_HASHES,
                Key::Skip(_) => COL_SKIP,
                Key::VersionBitState(_, _) => COL_VERSION_BIT_STATE,
            }
        }
    }
}
