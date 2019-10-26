mod codec;
mod constants;
mod helper;
mod peer;

use {
    log::{debug, LevelFilter},
    peer::{Peer, PeerOptions},
    std::{env, net::SocketAddr},
    tokio::net::TcpListener,
};

#[tokio::main]
async fn main() -> Result<(), failure::Error> {
    env_logger::builder()
        .filter_module("rust_bitcoin_node::peer", LevelFilter::Debug)
        .filter_module("rust_bitcoin_node", LevelFilter::Debug)
        .format_timestamp_millis()
        .init();
    let mut listener = TcpListener::bind("127.0.0.1:0").await?;

    debug!("Listening on {}", listener.local_addr()?);

    let addrs: Vec<SocketAddr> = env::args()
        .skip(1)
        .map(|arg| arg.parse().unwrap())
        .collect();

    for addr in addrs {
        tokio::spawn(async move {
            match Peer::connect(&addr, PeerOptions::default()).await {
                Ok(mut peer) => match peer.start().await {
                    Ok(_) => debug!("Peer {} disconnected gracefully", addr),
                    Err(err) => debug!("Peer {} disconnected with an error {}", addr, err),
                },
                Err(err) => debug!("Couldn't connect to peer: {}", err),
            }
        });
    }
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                tokio::spawn(async move {
                    match Peer::from_stream(stream, PeerOptions::default()) {
                        Ok(mut peer) => match peer.start().await {
                            Ok(_) => debug!("Peer {} disconnected gracefully", addr),
                            Err(err) => debug!("Peer {} disconnected with an error {}", addr, err),
                        },
                        Err(err) => debug!("Couldn't connect to peer: {}", err),
                    }
                });
            }
            _ => (),
        }
    }
}
