use super::connection::{Connection, ConnectionListener, ConnectionMessage};
use crate::util::EmptyResult;
use bitcoin::network::message::NetworkMessage;
use failure::Error;
use log::trace;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};

/// Unique ID for a connected peer
pub type PeerId = usize;

/// Trait that handles peer events
#[async_trait::async_trait]
pub trait PeerListener: Send + Sync + 'static {
    // called when handshake is complete
    async fn handle_open(&self, _peer: &Peer);
    // called when disconnected from peer
    async fn handle_close(&self, _peer: &Peer, _connected: bool);
    // called when new packet received
    async fn handle_packet(&self, _peer: &Peer, _packet: NetworkMessage);
    // called when error encountered
    async fn handle_error(&self, _peer: &Peer, _error: &Error);
    // called when peer is banned
    async fn handle_ban(&self, _peer: &Peer);
    // called when connection is successful
    async fn handle_connect(&self, _peer: &Peer);
}

/// A remote node to send and receive P2P network messages
pub struct Peer {
    listener: Option<Arc<dyn PeerListener>>,
    pub addr: SocketAddr,
    connection_tx: Mutex<mpsc::Sender<ConnectionMessage>>,
}

impl Peer {
    pub fn from_outbound(addr: SocketAddr, listener: Option<Arc<dyn PeerListener>>) -> Arc<Self> {
        let (connection_tx, connection_rx) = mpsc::channel(1);
        let peer = Arc::new(Self {
            addr,
            connection_tx: Mutex::new(connection_tx),
            listener,
        });
        Connection::from_outbound(peer.clone(), connection_rx);
        peer
    }

    pub fn from_inbound(
        addr: SocketAddr,
        stream: TcpStream,
        listener: Option<Arc<dyn PeerListener>>,
    ) -> Arc<Self> {
        let (connection_tx, connection_rx) = mpsc::channel(1);
        let peer = Arc::new(Self {
            addr,
            connection_tx: Mutex::new(connection_tx),
            listener,
        });
        Connection::from_inbound(peer.clone(), stream, connection_rx);
        peer
    }

    pub async fn send_version(&self) -> EmptyResult {
        use crate::util::now;
        use bitcoin::network::address::Address;
        use bitcoin::network::constants::PROTOCOL_VERSION;
        use bitcoin::network::{constants::ServiceFlags, message_network::VersionMessage};
        use std::net::{IpAddr, Ipv4Addr};

        let version = VersionMessage {
            version: PROTOCOL_VERSION,
            services: ServiceFlags::NONE,
            timestamp: now() as i64,
            receiver: Address::new(&self.addr, ServiceFlags::NONE),
            sender: Address::new(
                &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
                ServiceFlags::NONE,
            ),
            nonce: 0,
            user_agent: "/rust_bitcoin_node:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        };
        self.send(NetworkMessage::Version(version)).await
    }

    pub async fn send(&self, packet: NetworkMessage) -> EmptyResult {
        let command = packet.command();
        self.connection_tx
            .lock()
            .await
            .send(ConnectionMessage::Broadcast(packet))
            .await?;
        trace!("sent {} packet to {}", command, self.addr);
        Ok(())
    }

    async fn destroy(&self) {
        if let Some(listener) = &self.listener {
            listener.handle_close(self, false).await;
        }
    }

    async fn error(&self, error: &Error) {
        if let Some(listener) = &self.listener {
            listener.handle_error(self, error).await;
        }
    }
}

#[async_trait::async_trait]
impl ConnectionListener for Peer {
    // first step once connected to a peer is to send them a version packet
    async fn handle_connect(&self) {
        if let Some(listener) = &self.listener {
            listener.handle_connect(self).await;
        }

        if let Err(error) = self.send_version().await {
            self.error(&error).await;
            self.destroy().await;
        }
    }

    // destroy peer when connection is closed
    async fn handle_close(&self) {
        self.destroy().await;
    }

    // handle packet internally first and then let listeners handle
    // version, verack, ping and pong can be handled exclusively
    // internally but emit them anyway for completeness
    async fn handle_packet(&self, packet: NetworkMessage) {
        trace!("received {} packet from {}", packet.command(), self.addr);
        let res = match &packet {
            NetworkMessage::Version(_) => self.send(NetworkMessage::Verack).await,
            NetworkMessage::Ping(nonce) => self.send(NetworkMessage::Pong(*nonce)).await,
            _ => Ok(()),
        };
        if let Err(error) = res {
            self.handle_error(&error).await;
            return;
        }
        if let Some(listener) = &self.listener {
            listener.handle_packet(self, packet).await;
        }
    }

    // connection can error when initiating tcp stream, parsing packets or writing to the tcp stream
    async fn handle_error(&self, error: &Error) {
        self.error(error).await;
        self.destroy().await;
    }
}
