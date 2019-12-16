use log::LevelFilter;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_module("rust_bitcoin_node", LevelFilter::Trace)
        .format_timestamp_millis()
        .init();

    tokio::signal::ctrl_c().await?;

    Ok(())
}
