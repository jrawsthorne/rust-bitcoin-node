use bitcoin::{
    consensus::{Decodable, Encodable},
    p2p::{
        message::{NetworkMessage, RawNetworkMessage},
        Magic,
    },
};
use log::info;
use rust_bitcoin_node::net::{Peer, PeerEvent};
use std::{
    io::Write,
    net::{TcpStream, ToSocketAddrs},
};

fn main() {
    env_logger::init();

    let addr = "seed.bitcoin.sipa.be:8333"
        .to_socket_addrs()
        .unwrap()
        .next()
        .unwrap();

    println!("addr: {:?}", addr);

    info!("connecting to: {:?}", addr);

    let mut peer = Peer::new(addr);

    let mut stream = TcpStream::connect(addr).unwrap();

    peer.handle_event(PeerEvent::PeerConnected);

    loop {
        for event in peer.drain_outbox() {
            match event {
                PeerEvent::SendMessage(message) => {
                    send_message(message, &mut stream);
                }
                _ => {}
            }
        }

        let msg = read_message(&mut stream);
        peer.handle_event(PeerEvent::ReceivedMessage(msg));
    }
}

fn send_message(message: NetworkMessage, stream: &mut TcpStream) {
    info!("sending message: {:?}", message);
    let raw = RawNetworkMessage::new(Magic::BITCOIN, message);

    let mut buf = vec![];

    raw.consensus_encode(&mut buf).unwrap();

    stream.write_all(&buf).unwrap();
}

fn read_message(stream: &mut TcpStream) -> NetworkMessage {
    let msg = RawNetworkMessage::consensus_decode(stream)
        .unwrap()
        .into_payload();
    info!("received message: {:?}", msg);
    return msg;
}
