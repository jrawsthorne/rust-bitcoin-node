use log::LevelFilter;
use rust_bitcoin_node::{net::P2P, protocol::NetworkParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_module("rust_bitcoin_node", LevelFilter::Trace)
        .format_timestamp_millis()
        .init();
    P2P::new(NetworkParams::default(), vec![], 8);
    tokio::signal::ctrl_c().await?;

    Ok(())
}
