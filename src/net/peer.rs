use bitcoin::p2p::{
    message::NetworkMessage, message_network::VersionMessage, Address, ServiceFlags,
};
use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

pub struct Peer {
    addr: SocketAddr,
    handshake_state: Option<HandshakeState>,
    send_addrv2: bool,
    tx_relay: TxRelay,
    outbox: VecDeque<PeerEvent>,
    relay: bool,
    version: Option<u32>,
    services: ServiceFlags,
    user_agent: String,
    start_height: i32,
}

pub enum PeerEvent {
    PeerConnected,
    Connect(SocketAddr),
    SendMessage(NetworkMessage),
    ReceivedMessage(NetworkMessage),
}

enum HandshakeState {
    AwaitingVersion,
    AwaitingVerAck,
    Complete,
}

pub enum TxRelay {
    Txid,
    Wtxid,
}

const WTXID_RELAY_VERSION: u32 = 70016;

impl Peer {
    pub fn new(addr: SocketAddr) -> Peer {
        Peer {
            addr,
            handshake_state: None,
            outbox: VecDeque::new(),
            send_addrv2: false,
            tx_relay: TxRelay::Txid,
            relay: false,
            version: None,
            services: ServiceFlags::NONE,
            user_agent: String::new(),
            start_height: 0,
        }
    }

    pub fn handle_event(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::PeerConnected => {
                self.handle_peer_connected();
            }
            PeerEvent::ReceivedMessage(message) => {
                self.handle_received_message(message);
            }
            _ => {}
        }
    }

    fn handle_received_message(&mut self, message: NetworkMessage) {
        match message {
            NetworkMessage::Version(version) => {
                self.handle_version(version);
            }
            NetworkMessage::Verack => {
                self.handle_verack();
            }
            NetworkMessage::SendAddrV2 => {
                self.handle_send_addrv2();
            }
            NetworkMessage::WtxidRelay => {
                self.handle_wtxid_relay();
            }
            _ => {}
        }
    }

    fn handle_version(&mut self, version: VersionMessage) {
        if !matches!(self.handshake_state, Some(HandshakeState::AwaitingVersion)) {
            panic!("received version before awaiting version");
        }

        self.version = Some(version.version);
        self.services = version.services;
        self.user_agent = version.user_agent;
        self.relay = version.relay;
        self.start_height = version.start_height;

        if !self.services.has(ServiceFlags::NETWORK) {
            panic!("not full node");
        }

        if !self.services.has(ServiceFlags::WITNESS) {
            panic!("not segwit node");
        }

        self.handshake_state = Some(HandshakeState::AwaitingVerAck);

        self.send_message(NetworkMessage::Verack);

        if version.version >= WTXID_RELAY_VERSION {
            self.send_message(NetworkMessage::WtxidRelay);
        }

        self.send_message(NetworkMessage::SendAddrV2);
    }

    fn handle_verack(&mut self) {
        if !matches!(self.handshake_state, Some(HandshakeState::AwaitingVerAck)) {
            panic!("received verack before awaiting verack");
        }
        self.handshake_state = Some(HandshakeState::Complete);
    }

    fn handle_send_addrv2(&mut self) {
        self.send_addrv2 = true;
    }

    fn handle_wtxid_relay(&mut self) {
        self.tx_relay = TxRelay::Wtxid;
    }

    fn handle_peer_connected(&mut self) {
        let version = NetworkMessage::Version(VersionMessage {
            version: 70016,
            services: ServiceFlags::WITNESS
                | ServiceFlags::BLOOM
                | ServiceFlags::NETWORK
                | ServiceFlags::COMPACT_FILTERS
                | ServiceFlags::NETWORK_LIMITED,
            timestamp: 0,
            receiver: Address::new(&self.addr, ServiceFlags::NONE),
            sender: Address::new(
                &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
                ServiceFlags::NONE,
            ),
            nonce: 0,
            user_agent: "/rust-bitcoin-node:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        });
        self.handshake_state = Some(HandshakeState::AwaitingVersion);
        self.send_message(version);
    }

    pub fn drain_outbox<'a>(&'a mut self) -> impl Iterator<Item = PeerEvent> + 'a {
        self.outbox.drain(..)
    }

    fn send_message(&mut self, message: NetworkMessage) {
        self.outbox.push_back(PeerEvent::SendMessage(message));
    }
}
