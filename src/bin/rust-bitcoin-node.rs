use bitcoin::Network;
use rust_bitcoin_node::{net::Peer, Config, FullNode};
use std::net::{TcpListener, ToSocketAddrs};

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
    let addr = args
        .get(3)
        .expect("must enter an addr")
        .to_socket_addrs()
        .expect("must enter a valid addr")
        .next()
        .expect("must enter a valid addr");

    let full_node = FullNode::new(Config {
        path: path.into(),
        network: Network::Bitcoin,
        mempool: false,
        addr_index: false,
        filter_index: false,
        tx_index: false,
        verify_scripts: false,
    });

    let pool = &full_node.pool;

    let peer = Peer::new_outbound(addr, pool.clone(), network);

    peer.send_version();

    let listener = TcpListener::bind("127.0.0.1:55555").unwrap();

    loop {
        let inbound_peer = listener.incoming().next().unwrap().unwrap();

        Peer::new_inbound(inbound_peer, pool.clone(), network);
    }
}
