use bitcoin::Network;
use rust_bitcoin_node::{net::peer_manager::maintain_peers, Config, FullNode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder().format_timestamp_millis().init();

    let args = std::env::args().collect::<Vec<_>>();

    let path = args.get(1).expect("must enter a path");
    let network = match args
        .get(2)
        .expect("must enter a network (main, test, reg)")
        .as_ref()
    {
        "main" => Network::Bitcoin,
        "test" => Network::Testnet,
        "reg" => Network::Regtest,
        _ => panic!("must enter a valid network (main, test, reg)"),
    };

    let full_node = FullNode::new(Config {
        path: path.into(),
        network,
        mempool: true,
        addr_index: false,
        filter_index: false,
        tx_index: false,
        verify_scripts: false,
    });

    maintain_peers(full_node.pool.clone());

    Ok(())
}
