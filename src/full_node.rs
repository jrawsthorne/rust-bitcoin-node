use crate::{
    blockchain::{Chain, ChainOptions},
    indexers::{AddrIndexer, FilterIndexer, TxIndexer},
    mempool::MemPool,
    net::Pool,
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
    pub pool: Arc<Pool>,
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
        let chain = Arc::new(RwLock::new(
            Chain::new(ChainOptions::new(
                config.network,
                config.path.clone(),
                config.verify_scripts,
            ))
            .unwrap(),
        ));
        let mempool = if config.mempool {
            let mempool = Arc::new(RwLock::new(MemPool::new()));

            {
                let mempool = mempool.clone();
                chain
                    .write()
                    .on_connect(move |_chain, entry, block, _view| {
                        mempool.write().add_block(&block.txdata, entry.height);
                    });
            }

            Some(mempool)
        } else {
            None
        };
        let filter_index = if config.filter_index {
            let filter_index = Arc::new(RwLock::new(FilterIndexer::new(
                config.network,
                config.path.join("index/filters"),
            )));

            {
                let filter_index = filter_index.clone();
                chain.write().on_connect(move |_chain, entry, block, view| {
                    filter_index.write().handle_connect(entry, block, view);
                });
            }

            Some(filter_index)
        } else {
            None
        };
        let addr_index = if config.addr_index {
            let addr_index = Arc::new(RwLock::new(AddrIndexer::new(
                config.network,
                config.path.join("index/addr"),
            )));

            {
                let addr_index = addr_index.clone();
                chain.write().on_connect(move |_chain, entry, block, view| {
                    addr_index.write().index_block(entry, block, view);
                });
            }

            Some(addr_index)
        } else {
            None
        };
        let tx_index = if config.tx_index {
            let tx_index = Arc::new(RwLock::new(TxIndexer::new(config.path.join("index/tx"))));

            {
                let tx_index = tx_index.clone();
                chain
                    .write()
                    .on_connect(move |_chain, entry, block, _view| {
                        tx_index.write().index_block(entry, block);
                    });
            }

            Some(tx_index)
        } else {
            None
        };
        let pool = Pool::new(chain.clone(), mempool.clone(), filter_index.clone());
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
