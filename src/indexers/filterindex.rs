use crate::{
    blockchain::ChainListener,
    db::{Batch, DBKey, Database, DiskDatabase},
    ChainEntry, CoinView,
};
use bitcoin::{
    blockdata::constants::genesis_block,
    consensus::Encodable,
    hashes::Hash,
    util::bip158::{self, BlockFilter},
    Block, BlockHash, FilterHash, Network, OutPoint,
};
use log::info;
use parking_lot::RwLock;
use std::{path::Path, sync::Arc};

pub const COL_FILTER: &str = "F";
pub const COL_FILTER_HASH: &str = "H";
pub const COL_FILTER_HEADER: &str = "h";
pub const COL_HEIGHT: &str = "H";

// hash -> height
// height -> filter
// height -> filter hash
// height -> filter header

pub enum Key {
    Filter(u32),
    FilterHash(u32),
    FilterHeader(u32),
    Height(BlockHash),
}

impl DBKey for Key {
    fn encode(&self) -> Result<Vec<u8>, bitcoin::consensus::encode::Error> {
        let mut encoder = vec![];
        match self {
            Key::Filter(height) | Key::FilterHash(height) | Key::FilterHeader(height) => {
                height.consensus_encode(&mut encoder)?;
            }
            Key::Height(hash) => {
                hash.consensus_encode(&mut encoder)?;
            }
        }
        Ok(encoder)
    }

    fn col(&self) -> &'static str {
        match self {
            Key::Filter(_) => COL_FILTER,
            Key::FilterHash(_) => COL_FILTER_HASH,
            Key::FilterHeader(_) => COL_FILTER_HEADER,
            Key::Height(_) => COL_HEIGHT,
        }
    }
}

pub struct FilterIndexer {
    db: Arc<DiskDatabase<Key>>,
}

impl ChainListener for Arc<RwLock<FilterIndexer>> {
    fn handle_connect(
        &self,
        _chain: &crate::blockchain::Chain,
        entry: &ChainEntry,
        block: &Block,
        view: &CoinView,
    ) {
        self.write().block_connected(entry, block, view);
    }
}

impl FilterIndexer {
    pub fn new(network: Network, path: impl AsRef<Path>) -> Self {
        let mut filters = Self {
            db: Arc::new(DiskDatabase::new(
                path,
                vec![COL_FILTER, COL_FILTER_HASH, COL_FILTER_HEADER, COL_HEIGHT],
            )),
        };

        // add genesis if not added
        if !filters.db.has(Key::Filter(0)).unwrap() {
            info!("adding genesis to filter index");
            let genesis = genesis_block(network);
            let entry = ChainEntry::from_block(&genesis, None);
            filters.block_connected(&entry, &genesis, &CoinView::default());
        }

        filters
    }

    pub fn filter_header_by_height(&self, height: u32) -> Option<FilterHash> {
        self.db.get(Key::FilterHeader(height)).unwrap()
    }

    pub fn filter_by_height(&self, height: u32) -> Option<BlockFilter> {
        let content = self.db.get(Key::Filter(height)).unwrap()?;
        Some(BlockFilter { content })
    }

    pub fn filter_hash_by_height(&self, height: u32) -> Option<FilterHash> {
        self.db.get(Key::FilterHash(height)).unwrap()
    }

    pub fn filter_header_by_hash(&self, hash: BlockHash) -> Option<FilterHash> {
        let height = self.db.get(Key::Height(hash)).unwrap()?;
        self.filter_header_by_height(height)
    }

    pub fn filter_by_hash(&self, hash: BlockHash) -> Option<BlockFilter> {
        let height = self.db.get(Key::Height(hash)).unwrap()?;
        self.filter_by_height(height)
    }

    pub fn filter_hash_by_block_hash(&self, hash: BlockHash) -> Option<FilterHash> {
        let height = self.db.get(Key::Height(hash)).unwrap()?;
        self.filter_hash_by_height(height)
    }

    pub fn filters_by_block_hashes(&self, hashes: &[BlockHash]) -> Vec<BlockFilter> {
        hashes
            .iter()
            .map(|h| self.filter_by_hash(*h).unwrap())
            .collect()
    }

    pub fn filter_hashes_by_block_hashes(&self, hashes: &[BlockHash]) -> Vec<FilterHash> {
        hashes
            .iter()
            .map(|h| self.filter_hash_by_block_hash(*h).unwrap())
            .collect()
    }
}

impl FilterIndexer {
    pub fn block_connected(&mut self, entry: &ChainEntry, block: &Block, view: &CoinView) {
        let scripts_for_coin = |outpoint: &OutPoint| {
            view.map
                .get(outpoint)
                .map(|coin| &coin.output.script_pubkey)
                .ok_or_else(|| bip158::Error::UtxoMissing(*outpoint))
        };

        let filter = BlockFilter::new_script_filter(block, scripts_for_coin).unwrap();
        let filter_hash = FilterHash::hash(filter.content.as_slice());

        let prev_filter_header = if entry.prev_block == Default::default() {
            Default::default()
        } else {
            self.filter_header_by_hash(entry.prev_block).unwrap()
        };

        let filter_header = filter.filter_id(&prev_filter_header);

        let mut batch = Batch::new();

        batch
            .insert(Key::Height(entry.hash), &entry.height)
            .unwrap();
        batch
            .insert(Key::FilterHeader(entry.height), &filter_header)
            .unwrap();
        batch
            .insert(Key::FilterHash(entry.height), &filter_hash)
            .unwrap();
        batch
            .insert(Key::Filter(entry.height), &filter.content)
            .unwrap();

        self.db.write_batch(batch).unwrap();
    }
}
