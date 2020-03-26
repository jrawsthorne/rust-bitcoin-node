use log::LevelFilter;
use rust_bitcoin_node::{
    blockchain::{Chain, ChainOptions},
    net::P2P,
    protocol::NetworkParams,
};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_module("rust_bitcoin_node", LevelFilter::Debug)
        .format_timestamp_millis()
        .init();

    let mut chain = Chain::new(ChainOptions {
        network: NetworkParams::from_network(bitcoin::Network::Bitcoin),
        path: PathBuf::from_str("./data").unwrap(),
    });
    chain.open().await.unwrap();

    let chain = Arc::new(Mutex::new(chain));

    P2P::new(
        chain,
        NetworkParams::from_network(bitcoin::Network::Bitcoin),
        vec![],
        8,
    );

    tokio::signal::ctrl_c().await?;

    Ok(())
}
