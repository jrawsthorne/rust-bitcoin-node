use super::ChainEntry;
use crate::coins::{CoinEntry, CoinView};
use crate::db::{Batch, Database, DiskDatabase, Key};
use crate::protocol::NetworkParams;
use crate::util::EmptyResult;
use bitcoin::{hashes::sha256d::Hash as H256, BitcoinHash, Block, OutPoint, TxOut};
use failure::{bail, ensure, Error};
use log::info;
use std::collections::{hash_map::Entry, HashMap};

#[derive(Default)]
pub struct ChainEntryCache {
    height: HashMap<u32, ChainEntry>,
    hash: HashMap<H256, ChainEntry>,
    batch: Vec<ChainEntryCacheOperation>,
}

enum ChainEntryCacheOperation {
    InsertHash(ChainEntry),
    InsertHeight(ChainEntry),
    RemoveHash(H256),
    RemoveHeight(u32),
}

impl ChainEntryCache {
    fn insert_height_batch(&mut self, entry: &ChainEntry) {
        self.batch
            .push(ChainEntryCacheOperation::InsertHeight(entry.clone()));
    }

    fn insert_hash_batch(&mut self, entry: &ChainEntry) {
        self.batch
            .push(ChainEntryCacheOperation::InsertHash(entry.clone()));
    }

    fn remove_height_batch(&mut self, entry: &ChainEntry) {
        self.batch
            .push(ChainEntryCacheOperation::RemoveHeight(entry.height));
    }

    fn remove_hash_batch(&mut self, entry: &ChainEntry) {
        self.batch
            .push(ChainEntryCacheOperation::RemoveHash(entry.hash));
    }

    fn write_batch(&mut self) {
        use ChainEntryCacheOperation::*;
        for operation in self.batch.drain(..) {
            match operation {
                InsertHash(entry) => self.hash.insert(entry.hash, entry),
                InsertHeight(entry) => self.height.insert(entry.height, entry.clone()),
                RemoveHeight(height) => self.height.remove(&height),
                RemoveHash(hash) => self.hash.remove(&hash),
            };
        }
    }

    fn clear_batch(&mut self) {
        self.batch.clear();
    }
}

enum CoinCacheOperation {
    Insert(H256, u32, CoinEntry),
    Remove(H256, u32),
}

#[derive(Default)]
pub struct CoinCache {
    coins: HashMap<H256, HashMap<u32, CoinEntry>>,
    batch: Vec<CoinCacheOperation>,
}

impl CoinCache {
    fn insert(&mut self, hash: H256, index: u32, coin_entry: &CoinEntry) {
        match self.coins.entry(hash) {
            Entry::Occupied(mut entry) => {
                let coins = entry.get_mut();
                coins.insert(index, coin_entry.clone());
            }
            Entry::Vacant(entry) => {
                let mut coins = HashMap::new();
                coins.insert(index, coin_entry.clone());
                entry.insert(coins);
            }
        }
    }

    fn remove(&mut self, hash: H256, index: &u32) {
        match self.coins.entry(hash) {
            Entry::Occupied(mut entry) => {
                let coins = entry.get_mut();
                coins.remove(index);
                if coins.is_empty() {
                    entry.remove();
                }
            }
            _ => (),
        }
    }

    fn insert_batch(&mut self, hash: H256, index: u32, coin_entry: &CoinEntry) {
        self.batch
            .push(CoinCacheOperation::Insert(hash, index, coin_entry.clone()));
    }

    fn remove_batch(&mut self, hash: H256, index: u32) {
        self.batch.push(CoinCacheOperation::Remove(hash, index));
    }

    fn write_batch(&mut self) {
        use CoinCacheOperation::*;
        for operation in self.batch.drain(..).collect::<Vec<_>>() {
            match operation {
                Insert(hash, index, entry) => self.insert(hash, index, &entry),
                Remove(hash, index) => self.remove(hash, &index),
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
    db: DiskDatabase,
    /// Allows for atomic writes to the database
    current: Option<Batch>,
    /// Allows for atomic chain state update
    pending: Option<ChainState>,
    network_params: NetworkParams,
}

pub struct ChainDBOptions {
    pub path: std::path::PathBuf,
}

impl ChainDB {
    pub fn new(network_params: NetworkParams, options: ChainDBOptions) -> Self {
        Self {
            network_params,
            db: DiskDatabase::new(options.path),
            coin_cache: Default::default(),
            entry_cache: Default::default(),
            state: Default::default(),
            current: Default::default(),
            pending: Default::default(),
        }
    }

    pub fn open(&mut self) -> EmptyResult {
        match self.db.get(Key::ChainState)? {
            Some(state) => self.state = state,
            None => {
                self.save_genesis()?;
            }
        }
        Ok(())
    }

    fn save_genesis(&mut self) -> EmptyResult {
        use bitcoin::blockdata::constants::genesis_block;
        let genesis = genesis_block(self.network_params.network);
        let entry = ChainEntry::from_block(&genesis, None);
        let view = CoinView::default();
        info!("Writing genesis block to ChainDB.");
        self.save(&entry, &genesis, Some(&view))
    }

    fn batch(&mut self) -> Result<&mut Batch, Error> {
        ensure!(self.current.is_some());
        Ok(self.current.as_mut().unwrap())
    }

    fn commit(&mut self) -> EmptyResult {
        ensure!(self.current.is_some());
        ensure!(self.pending.is_some());

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

        Ok(())
    }

    fn drop(&mut self) -> EmptyResult {
        ensure!(self.current.is_some());
        ensure!(self.pending.is_some());

        self.current.take();
        self.pending.take();

        self.entry_cache.clear_batch();
        self.coin_cache.clear_batch();

        Ok(())
    }

    pub fn get_tip(&mut self) -> Result<Option<&ChainEntry>, Error> {
        let tip = self.state.tip;
        self.get_entry_by_hash(tip)
    }

    pub fn get_entry_by_hash(&mut self, hash: H256) -> Result<Option<&ChainEntry>, Error> {
        Ok(match self.entry_cache.hash.entry(hash) {
            Entry::Occupied(entry) => Some(entry.into_mut()),
            Entry::Vacant(entry) => {
                if let Some(chain_entry) = self.db.get(Key::ChainEntry(hash))? {
                    Some(entry.insert(chain_entry))
                } else {
                    None
                }
            }
        })
    }

    pub fn read_coin(&mut self, prevout: &OutPoint) -> Result<Option<&CoinEntry>, Error> {
        Ok(match self.coin_cache.coins.entry(prevout.txid) {
            Entry::Occupied(entry) => {
                let coins = entry.into_mut();
                match coins.entry(prevout.vout) {
                    Entry::Occupied(coin_entry) => Some(coin_entry.into_mut()),
                    Entry::Vacant(entry) => {
                        if let Some(coin) = self.db.get(Key::Coin(prevout.txid, prevout.vout))? {
                            Some(entry.insert(coin))
                        } else {
                            None
                        }
                    }
                }
            }
            Entry::Vacant(entry) => {
                if let Some(coin) = self.db.get(Key::Coin(prevout.txid, prevout.vout))? {
                    let mut coins = HashMap::new();
                    coins.insert(prevout.vout, coin);
                    entry.insert(coins).get(&prevout.vout)
                } else {
                    None
                }
            }
        })
    }

    pub fn save(
        &mut self,
        entry: &ChainEntry,
        block: &Block,
        view: Option<&CoinView>,
    ) -> EmptyResult {
        self.start()?;
        if let Err(error) = self._save(entry, block, view) {
            self.drop()?;
            Err(error)
        } else {
            self.commit()?;
            Ok(())
        }
    }

    fn _save(&mut self, entry: &ChainEntry, block: &Block, view: Option<&CoinView>) -> EmptyResult {
        let hash = block.bitcoin_hash();

        self.batch()?
            .insert(Key::ChainEntryHeight(hash), &entry.height)?;
        self.batch()?.insert(Key::ChainEntry(hash), entry)?;
        self.entry_cache.insert_hash_batch(entry);

        if view.is_none() {
            return self.save_block(entry, block, None);
        }

        if !entry.is_genesis() {
            self.batch()?
                .insert(Key::ChainNextHash(entry.prev_block), &hash)?;
        }

        self.batch()?
            .insert(Key::ChainEntryHash(entry.height), &hash)?;
        self.entry_cache.insert_height_batch(entry);

        self.save_block(entry, block, view)?;

        let chain_state = self.pending.as_mut().unwrap().commit(hash).clone();
        self.batch()?.insert(Key::ChainState, &chain_state)
    }

    fn start(&mut self) -> EmptyResult {
        ensure!(self.current.is_none());
        ensure!(self.pending.is_none());

        self.current.replace(Batch::default());
        self.pending.replace(self.state.clone());

        self.entry_cache.clear_batch();

        Ok(())
    }

    fn save_block(
        &mut self,
        entry: &ChainEntry,
        block: &Block,
        view: Option<&CoinView>,
    ) -> EmptyResult {
        // TODO: Write block to disk

        if let Some(view) = view {
            self.connect_block(entry, block, view)
        } else {
            Ok(())
        }
    }
    fn connect_block(&mut self, entry: &ChainEntry, block: &Block, view: &CoinView) -> EmptyResult {
        let pending = self.pending.as_mut().unwrap();
        pending.connect(block);

        if entry.is_genesis() {
            return Ok(());
        }

        for (i, tx) in block.txdata.iter().enumerate() {
            if i > 0 {
                for input in &tx.input {
                    if let Some(output) = view.get_output(&input.previous_output) {
                        pending.spend(output);
                    } else {
                        bail!("prev out not in coin view");
                    }
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
    fn save_view(&mut self, view: &CoinView) -> EmptyResult {
        for (hash, coins) in &view.map {
            for (index, coin) in &coins.outputs {
                if coin.spent {
                    self.coin_cache.remove_batch(*hash, *index);
                    self.batch()?.remove(Key::Coin(*hash, *index))?;
                    continue;
                }
                self.coin_cache.insert_batch(*hash, *index, coin);
                self.batch()?.insert(Key::Coin(*hash, *index), coin)?;
            }
        }
        Ok(())
    }
}

/// Current state of the main chain
#[derive(Default, Clone, Debug)]
pub struct ChainState {
    /// Hash of the tip of the chain
    pub tip: H256,
    /// Number of transactions that have occurred
    pub tx: usize,
    /// Nnumber of unspent coins
    pub coin: usize,
    /// The value of unspent coins
    pub value: u64,
    pub commited: bool,
}

impl ChainState {
    fn connect(&mut self, block: &Block) {
        self.tx += block.txdata.len();
    }

    fn add(&mut self, coin: &TxOut) {
        self.coin += 1;
        self.value += coin.value;
    }

    fn spend(&mut self, coin: &TxOut) {
        self.coin -= 1;
        self.value -= coin.value;
    }

    fn commit(&mut self, hash: H256) -> &ChainState {
        self.tip = hash;
        self.commited = true;
        self
    }
}
