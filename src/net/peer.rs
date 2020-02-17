use super::connection::Connection;
use bitcoin::{
    network::{
        constants::ServiceFlags, message::NetworkMessage, message_blockdata::GetHeadersMessage,
        message_network::VersionMessage,
    },
    BlockHash, Network,
};
use failure::Error;
use log::{debug, error, trace};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;

/// Unique ID for a connected peer
pub type PeerId = usize;

#[derive(Debug, Eq, PartialEq, Hash)]
enum ResponseType {
    GetHeaders,
    GetData,
}

/// A remote node to send and receive P2P network messages
pub struct Peer {
    p2p: P2PManager,
    pub addr: SocketAddr,
    connection_tx: mpsc::UnboundedSender<NetworkMessage>,
    close_connection_tx: mpsc::UnboundedSender<()>,
    pub id: PeerId,
    pub block_map: Mutex<HashMap<BlockHash, u32>>,
    pub stalling_since: AtomicUsize,
    pub best: Mutex<Best>,
    pub common: Mutex<Common>,
    pub syncing: AtomicBool,
    pub destroyed: AtomicBool,
    response_map: Mutex<HashMap<ResponseType, u64>>,
    pub handshake: AtomicBool,
    pub connected: AtomicBool,
    pub destroy: AtomicBool,
    pub block_time: AtomicUsize,
}

#[derive(Default)]
pub struct Common {
    pub height: Option<u32>,
    pub hash: Option<BlockHash>,
}

#[derive(Default)]
pub struct Best {
    pub hash: Option<BlockHash>,
    pub height: Option<u32>,
}

use super::p2p::P2PManager;

impl Peer {
    pub fn from_outbound(
        network: Network,
        id: PeerId,
        addr: SocketAddr,
        p2p: P2PManager,
    ) -> Arc<Self> {
        let (peer, connection_rx, close_connection_rx) = Self::create_peer(addr, id, p2p);
        Connection::from_outbound(network, peer.clone(), connection_rx, close_connection_rx);
        peer
    }

    fn create_peer(
        addr: SocketAddr,
        id: PeerId,
        p2p: P2PManager,
    ) -> (
        Arc<Peer>,
        mpsc::UnboundedReceiver<NetworkMessage>,
        mpsc::UnboundedReceiver<()>,
    ) {
        let (connection_tx, connection_rx) = mpsc::unbounded_channel();
        let (close_connection_tx, close_connection_rx) = mpsc::unbounded_channel();
        let peer = Arc::new(Self {
            addr,
            connection_tx,
            p2p,
            id,
            block_map: Default::default(),
            stalling_since: Default::default(),
            best: Default::default(),
            common: Default::default(),
            syncing: Default::default(),
            destroyed: Default::default(),
            close_connection_tx,
            response_map: Default::default(),
            handshake: Default::default(),
            connected: Default::default(),
            destroy: Default::default(),
            block_time: Default::default(),
        });
        Arc::clone(&peer).stall_timer();
        (peer, connection_rx, close_connection_rx)
    }

    fn stall_timer(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5));

            // TODO: messy
            let mut connected = false;
            let mut connected_time = 0;
            let mut handshake = false;

            loop {
                interval.tick().await;

                if self.destroyed.load(Ordering::SeqCst) {
                    return;
                }

                if self.destroy.load(Ordering::SeqCst) {
                    debug!("was told to destroy {}", self.addr);
                    self.destroy().await;
                    return;
                }

                let now = crate::util::now();

                if !connected {
                    if self.connected.load(Ordering::SeqCst) {
                        connected = true;
                        connected_time = now;
                    } else {
                        continue;
                    }
                }

                if !handshake && self.handshake.load(Ordering::SeqCst) {
                    handshake = true;
                }

                if !handshake && now > connected_time + 10 {
                    error!("Handshake timeout {}", self.addr);
                    self.destroy().await;
                    return;
                }

                {
                    let response_map = self.response_map.lock().await;
                    for (item, timeout) in response_map.iter() {
                        if &now > timeout {
                            error!("Peer is stalling {:?} {}", item, self.addr);
                            self.destroy().await;
                            return;
                        }
                    }
                }

                {
                    let block_map = self.block_map.lock().await;
                    if self.syncing.load(Ordering::SeqCst)
                        && block_map.len() > 0
                        && now as usize > self.block_time.load(Ordering::SeqCst) + 120
                    {
                        debug!("Peer is stalling (block). {}", self.addr);
                        self.destroy().await;
                        return;
                    }
                }

                let stalling_since = self.stalling_since.load(Ordering::SeqCst);

                if stalling_since > 0 && now as usize > stalling_since + 2 {
                    debug!("Peer is stalling block download window");
                    self.destroy().await;
                    return;
                }
            }
        });
    }

    pub fn from_inbound(
        network: Network,
        id: PeerId,
        addr: SocketAddr,
        stream: TcpStream,
        p2p: P2PManager,
    ) -> Arc<Self> {
        let (peer, connection_rx, close_connection_rx) = Self::create_peer(addr, id, p2p);
        Connection::from_inbound(
            network,
            peer.clone(),
            stream,
            connection_rx,
            close_connection_rx,
        );
        peer
    }

    pub async fn send_version(&self) {
        use crate::util::now;
        use bitcoin::network::address::Address;
        use bitcoin::network::constants::PROTOCOL_VERSION;
        use std::net::{IpAddr, Ipv4Addr};

        let version = VersionMessage {
            version: PROTOCOL_VERSION,
            services: ServiceFlags::NETWORK | ServiceFlags::WITNESS,
            timestamp: now() as i64,
            receiver: Address::new(&self.addr, ServiceFlags::NONE),
            sender: Address::new(
                &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
                ServiceFlags::NONE,
            ),
            nonce: 0,
            user_agent: "/rust_bitcoin_node:0.1.0/".to_string(),
            start_height: 0,
            relay: false,
        };
        self.send(NetworkMessage::Version(version)).await;
    }

    async fn add_timeout(&self, packet: &NetworkMessage) {
        let timeout = 30;
        use crate::util::now;

        match packet {
            NetworkMessage::GetHeaders(_) => {
                self.response_map
                    .lock()
                    .await
                    .insert(ResponseType::GetHeaders, now() + timeout);
            }
            NetworkMessage::GetData(_) => {
                self.response_map
                    .lock()
                    .await
                    .insert(ResponseType::GetData, now() + (timeout * 2));
            }
            _ => (),
        }
    }

    pub async fn send(&self, packet: NetworkMessage) {
        if self.destroyed.load(Ordering::SeqCst) {
            return;
        }
        let command = packet.command();
        self.add_timeout(&packet).await;
        if self.connection_tx.send(packet).is_err() {
            // don't call destroy() because it could cause deadlock
            // if tried to send while holding peers read lock
            self.destroy.store(true, Ordering::SeqCst);
        } else {
            trace!("sent {} packet to {}", command, self.addr);
        }
    }

    pub async fn send_get_headers(&self, locator: Vec<BlockHash>, stop: Option<BlockHash>) {
        if locator.is_empty() {
            return;
        }
        debug!(
            "Requesting headers packet from peer with getheaders ({}).",
            self.addr
        );
        debug!("Sending getheaders (hash={}, stop={:?}).", locator[0], stop);
        self.send(NetworkMessage::GetHeaders(GetHeadersMessage::new(
            locator,
            stop.unwrap_or(BlockHash::default()),
        )))
        .await;
    }

    pub async fn destroy(&self) {
        if self.destroyed.load(Ordering::SeqCst) {
            trace!("{} already destroyed", self.addr);
            return;
        }

        trace!("destroying {}", self.addr);

        self.destroyed.store(true, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);

        let _ = self.close_connection_tx.send(());

        self.p2p.lock().await.handle_close(self, false).await;

        trace!("destroyed {}", self.addr);
    }

    async fn error(&self, error: &Error) {
        self.p2p.lock().await.handle_error(self, error).await;
    }

    async fn handle_version(&self, version: &VersionMessage) {
        let services = version.services;

        if services.has(ServiceFlags::NETWORK) {
            self.send(NetworkMessage::Verack).await;
        } else {
            self.destroy.store(true, Ordering::SeqCst);
        }
    }
}

impl Peer {
    // first step once connected to a peer is to send them a version packet
    pub async fn handle_connect(&self) {
        if self.destroyed.load(Ordering::SeqCst) {
            return;
        }

        self.connected.store(true, Ordering::SeqCst);

        self.p2p.lock().await.handle_connect(self).await;

        self.send_version().await;
    }

    // destroy peer when connection is closed
    pub async fn handle_close(&self) {
        self.destroy.store(true, Ordering::SeqCst);
    }

    // handle packet internally first and then let listeners handle
    // version, verack, ping and pong can be handled exclusively
    // internally but emit them anyway for completeness
    pub async fn handle_packet(&self, packet: NetworkMessage) {
        if self.destroyed.load(Ordering::SeqCst) || self.destroy.load(Ordering::SeqCst) {
            return;
        }

        match &packet {
            NetworkMessage::Block(_) => {
                self.response_map
                    .lock()
                    .await
                    .remove(&ResponseType::GetData);
            }
            NetworkMessage::Headers(_) => {
                self.response_map
                    .lock()
                    .await
                    .remove(&ResponseType::GetHeaders);
            }
            _ => (),
        };

        trace!("received {} packet from {}", packet.command(), self.addr);
        match &packet {
            NetworkMessage::Version(version) => self.handle_version(version).await,
            NetworkMessage::Ping(nonce) => self.send(NetworkMessage::Pong(*nonce)).await,
            NetworkMessage::Verack => {
                self.handshake.store(true, Ordering::SeqCst);
                self.p2p.lock().await.handle_open(self).await;
            }
            _ => (),
        };

        self.p2p.lock().await.handle_packet(self, packet).await;
    }

    // connection can error when initiating tcp stream, parsing packets or writing to the tcp stream
    pub async fn handle_error(&self, error: &Error) {
        if self.destroyed.load(Ordering::SeqCst) {
            return;
        }
        self.error(error).await;
        self.destroy.store(true, Ordering::SeqCst);
    }
}
