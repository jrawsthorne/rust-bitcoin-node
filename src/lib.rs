pub mod net;
// use bitcoin::consensus::{Decodable, Encodable};
// use bitcoin::p2p::message::RawNetworkMessage;
// use bitcoin::p2p::Magic;
// use bitcoin::p2p::{message::NetworkMessage, ServiceFlags};
// use bitcoin::p2p::{message_network::VersionMessage, Address};
// use std::io::Write;
// use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
// use std::time::Duration;

// fn main() {
//     let seeds = vec![
//         "seed.bitcoin.sipa.be", // Pieter Wuille, only supports x1, x5, x9, and xd
//         "dnsseed.bluematt.me",  // Matt Corallo, only supports x9
//         "dnsseed.bitcoin.dashjr.org", // Luke Dashjr
//         "seed.bitcoinstats.com", // Christian Decker, supports x1 - xf
//         "seed.bitcoin.jonasschnelli.ch", // Jonas Schnelli, only supports x1, x5, x9, and xd
//         "seed.btc.petertodd.org", // Peter Todd, only supports x1, x5, x9, and xd
//         "seed.bitcoin.sprovoost.nl", // Sjors Provoost
//         "dnsseed.emzy.de",      // Stephan Oeste
//     ];

//     for seed in seeds {
//         let addrs: Vec<_> = format!("{seed}:8333").to_socket_addrs().unwrap().collect();
//         for addr in addrs {
//             let stream = TcpStream::connect(addr).unwrap();
//             let copy = stream.try_clone().unwrap();
//             std::thread::spawn(move || read_loop(copy));
//             write_loop(addr, stream);
//         }
//     }
// }

// fn write_loop(addr: SocketAddr, mut stream: TcpStream) {
//     send_version(addr, &mut stream);
//     std::thread::sleep(Duration::from_secs(10));
// }

// fn send_message(msg: NetworkMessage, stream: &mut TcpStream) {
//     let raw = RawNetworkMessage::new(Magic::BITCOIN, msg);

//     let mut buf = vec![];

//     raw.consensus_encode(&mut buf).unwrap();

//     stream.write_all(buf.as_slice()).unwrap();
// }

// fn send_version(addr: SocketAddr, stream: &mut TcpStream) {
//     let version = NetworkMessage::Version(VersionMessage {
//         version: 70016,
//         services: ServiceFlags::WITNESS
//             | ServiceFlags::BLOOM
//             | ServiceFlags::NETWORK
//             | ServiceFlags::COMPACT_FILTERS
//             | ServiceFlags::NETWORK_LIMITED,
//         timestamp: 0,
//         receiver: Address::new(&addr, ServiceFlags::NONE),
//         sender: Address::new(
//             &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
//             ServiceFlags::NONE,
//         ),
//         nonce: 0,
//         user_agent: "/rust-bitcoin-node:0.1.0/".to_string(),
//         start_height: 0,
//         relay: true,
//     });
//     send_message(version, stream);
// }

// fn read_loop(stream: TcpStream) {
//     let mut stream = stream;
//     loop {
//         let msg = RawNetworkMessage::consensus_decode(&mut stream).unwrap();
//         println!("{:?}", msg);
//     }
// }
