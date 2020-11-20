use crate::{
    blockchain::{Chain, ChainOptions},
    indexers::{AddrIndexer, FilterIndexer, TxIndexer},
    mempool::MemPool,
    net::PeerManager,
    protocol::NetworkParams,
};
use bitcoin::Network;
use parking_lot::RwLock;
use std::{path::PathBuf, sync::Arc};

pub struct FullNode {
    pub chain: Arc<RwLock<Chain>>,
    pub mempool: Option<Arc<RwLock<MemPool>>>,
    pub tx_index: Option<Arc<RwLock<TxIndexer>>>,
    pub addr_index: Option<Arc<RwLock<AddrIndexer>>>,
    pub filter_index: Option<Arc<RwLock<FilterIndexer>>>,
    pub pool: Arc<PeerManager>,
}

pub struct Config {
    pub path: PathBuf,
    pub network: Network,
    pub mempool: bool,
    pub addr_index: bool,
    pub filter_index: bool,
    pub tx_index: bool,
    pub verify_scripts: bool,
}

impl FullNode {
    pub fn new(config: Config) -> Self {
        let mut chain = Chain::new(ChainOptions::new(
            config.network,
            config.path.clone(),
            config.verify_scripts,
        ))
        .unwrap();

        let mempool = if config.mempool {
            let mempool = Arc::new(RwLock::new(MemPool::new()));

            chain.add_listener(Arc::clone(&mempool));

            Some(mempool)
        } else {
            None
        };

        let filter_index = if config.filter_index {
            let filter_index = Arc::new(RwLock::new(FilterIndexer::new(
                config.network,
                config.path.join("index/filters"),
            )));

            chain.add_listener(Arc::clone(&filter_index));

            Some(filter_index)
        } else {
            None
        };

        let addr_index = if config.addr_index {
            let addr_index = Arc::new(RwLock::new(AddrIndexer::new(
                config.network,
                config.path.join("index/addr"),
            )));

            chain.add_listener(Arc::clone(&addr_index));

            Some(addr_index)
        } else {
            None
        };

        let tx_index = if config.tx_index {
            let tx_index = Arc::new(RwLock::new(TxIndexer::new(config.path.join("index/tx"))));

            chain.add_listener(Arc::clone(&tx_index));

            Some(tx_index)
        } else {
            None
        };

        let chain = Arc::new(RwLock::new(chain));

        let network = NetworkParams::from_network(config.network);

        let pool = PeerManager::new(
            Arc::clone(&chain),
            mempool.clone(),
            filter_index.clone(),
            network,
        );

        Self {
            chain,
            mempool,
            tx_index,
            addr_index,
            filter_index,
            pool,
        }
    }
}
