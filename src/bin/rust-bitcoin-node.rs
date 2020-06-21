use bitcoin::Network;
use log::LevelFilter;
use parking_lot::RwLock;
use rust_bitcoin_node::{
    blockchain::{Chain, ChainListener, ChainOptions},
    mempool::MemPool,
    net::newp2p::{SharedMempool, P2P},
};
use std::sync::Arc;

struct MemPoolChainListener {
    mempool: SharedMempool,
}

impl ChainListener for MemPoolChainListener {
    fn handle_connect(
        &self,
        _chain: &Chain,
        entry: &rust_bitcoin_node::ChainEntry,
        block: &bitcoin::Block,
        _view: &rust_bitcoin_node::CoinView,
    ) {
        let mut mempool = self.mempool.write();
        mempool.add_block(&block.txdata, entry.height);
    }
    fn handle_disconnect(
        &self,
        chain: &Chain,
        entry: &rust_bitcoin_node::ChainEntry,
        block: &bitcoin::Block,
        _view: &rust_bitcoin_node::CoinView,
    ) {
        let mut mempool = self.mempool.write();
        mempool.remove_block(chain, entry, &block.txdata);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_module("rust_bitcoin_node", LevelFilter::Debug)
        .format_timestamp_millis()
        .init();

    let network = Network::Testnet;
    let mempool = Arc::new(RwLock::new(MemPool::new()));

    let chain = Arc::new(RwLock::new(
        Chain::new(
            ChainOptions::new(network, "./data"),
            vec![Arc::new(MemPoolChainListener {
                mempool: mempool.clone(),
            })],
        )
        .unwrap(),
    ));

    P2P::new(chain, network, mempool, "127.0.0.1:18333");

    Ok(())
}
