use anyhow::Result;
use bitcoin::Network;
use parking_lot::RwLock;
use rust_bitcoin_node::{
    blockchain::{Chain, ChainOptions},
    net::new_peer_manager::PeerManager,
    protocol::NetworkParams,
};
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::builder().format_timestamp_millis().init();

    // let args = std::env::args().collect::<Vec<_>>();

    // let path = args.get(1).expect("must enter a path");
    // let network = match args
    //     .get(2)
    //     .expect("must enter a network (main, test, reg)")
    //     .as_ref()
    // {
    //     "main" => Network::Bitcoin,
    //     "test" => Network::Testnet,
    //     "reg" => Network::Regtest,
    //     _ => panic!("must enter a valid network (main, test, reg)"),
    // };

    // let full_node = FullNode::new(Config {
    //     path: path.into(),
    //     network,
    //     mempool: true,
    //     addr_index: false,
    //     filter_index: false,
    //     tx_index: false,
    //     verify_scripts: false,
    // });

    // maintain_peers(full_node.pool.clone());

    let network_params = NetworkParams::from_network(Network::Bitcoin);

    let chain = RwLock::new(
        Chain::new(ChainOptions {
            network: network_params.clone(),
            verify_scripts: true,
            path: "./data".into(),
        })
        .unwrap(),
    );
    let _peer_manager = PeerManager::new(8, network_params, chain);

    signal(SignalKind::terminate())
        .unwrap()
        .recv()
        .await
        .unwrap();

    Ok(())
}
