mod codec;
mod constants;
mod helper;
mod peer;
mod pool;

use {
    log::LevelFilter,
    pool::{Pool, PoolOptions},
    std::{env, net::SocketAddr},
};

#[tokio::main]
async fn main() -> Result<(), failure::Error> {
    env_logger::builder()
        .filter_module("rust_bitcoin_node::peer", LevelFilter::Debug)
        .filter_module("rust_bitcoin_node", LevelFilter::Debug)
        .format_timestamp_millis()
        .init();

    let addrs: Vec<SocketAddr> = env::args()
        .skip(1)
        .map(|arg| arg.parse().unwrap())
        .collect();

    let mut pool = Pool::connect(addrs, PoolOptions::default()).await?;

    pool.start().await?;

    Ok(())
}
