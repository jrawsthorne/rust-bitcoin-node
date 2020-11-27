use super::connection::{
    create_connection, DisconnectReceiver, DisconnectSender, MessageReceiver, WriteHandle,
};
use crate::{protocol::WTXID_RELAY_VERSION, util::now};
use anyhow::{bail, Result};
use bitcoin::{
    network::message_blockdata::GetHeadersMessage,
    network::{
        constants::ServiceFlags, message::NetworkMessage, message_compact_blocks::SendCmpct,
        message_network::VersionMessage,
    },
    BlockHash, Network,
};
use log::{debug, trace};
use parking_lot::Mutex;
use std::{collections::HashMap, net::SocketAddr};
use tokio::net::TcpStream;

pub struct Peer {
    pub addr: SocketAddr,
    write_handle: WriteHandle,
    pub state: Mutex<State>,
    outbound: bool,
    disconnect_tx: DisconnectSender,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("addr", &self.addr)
            .field("state", &self.state.lock())
            .field("outbound", &self.outbound)
            .finish()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct State {
    pub version: Option<u32>,
    pub services: ServiceFlags,
    pub start_height: i32,
    pub relay: bool,
    pub user_agent: String,
    pub addr_relay: AddrRelay,
    pub tx_relay: TxRelay,
    pub block_relay: BlockRelay,
    pub handshake: bool,
    pub ping_nonce_sent: Option<u64>,
    pub best_height: Option<u32>,
    pub best_hash: Option<BlockHash>,
    pub common_height: Option<u32>,
    pub common_hash: Option<BlockHash>,
    pub requested_blocks: HashMap<BlockHash, u64>,
    pub stalling_since: Option<u64>, // TODO: Use time library for timestamps
    pub block_time: Option<u64>,
    pub syncing: bool,
    pub disconnected: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TxRelay {
    Txid,
    Wtxid,
}

#[derive(Debug, Eq, PartialEq)]
pub enum AddrRelay {
    V1,
    V2,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum BlockRelay {
    Compact {
        witness: bool,
        relay: Option<CompactRelay>,
    },
    Headers,
    Inv,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum CompactRelay {
    Headers,
    Inv,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: None,
            services: ServiceFlags::NONE,
            start_height: 0,
            relay: false,
            user_agent: "".to_string(),
            addr_relay: AddrRelay::V1,
            tx_relay: TxRelay::Txid,
            block_relay: BlockRelay::Inv,
            handshake: false,
            ping_nonce_sent: None,
            best_hash: None,
            best_height: None,
            common_hash: None,
            common_height: None,
            requested_blocks: HashMap::new(),
            stalling_since: None,
            block_time: None,
            syncing: false,
            disconnected: false,
        }
    }
}

impl Peer {
    pub fn new(
        addr: SocketAddr,
        stream: TcpStream,
        network: Network,
    ) -> (Self, MessageReceiver, DisconnectReceiver) {
        let (write_handle, message_rx, (disconnect_tx, disconnect_rx)) =
            create_connection(addr, stream, network);
        let peer = Self {
            addr,
            write_handle,
            outbound: true,
            state: Default::default(),
            disconnect_tx,
        };
        (peer, message_rx, disconnect_rx)
    }

    pub fn disconnect(&self, error: Result<()>) {
        let mut state = self.state.lock();
        if state.disconnected {
            return;
        }
        state.disconnected = true;
        self.disconnect_tx.send(error).unwrap();
        trace!("sent disconnect signal ({})", self.addr);
    }

    pub async fn handshake(&self, event_rx: &mut MessageReceiver) -> Result<()> {
        if self.outbound {
            self.queue_version_message();
        }

        match event_rx.recv().await {
            Some(NetworkMessage::Version(version)) => self.handle_version(version)?,
            _ => bail!("unexpected handshake message"),
        }

        loop {
            match event_rx.recv().await {
                Some(NetworkMessage::Verack) => {
                    self.handle_verack();
                    break;
                }
                Some(NetworkMessage::WtxidRelay) => {
                    if matches!(self.state.lock().version, Some(v) if v >= WTXID_RELAY_VERSION) {
                        self.handle_wtxid_relay();
                    }
                }
                _ => bail!("unexpected handshake message"),
            }
        }

        Ok(())
    }

    pub fn queue_message(&self, message: NetworkMessage) {
        self.write_handle.queue_message(message);
    }

    pub async fn queue_message_sync(&self, message: NetworkMessage) {
        self.write_handle.write_message(message).await;
    }

    fn queue_version_message(&self) {
        use bitcoin::network::Address;

        let version = VersionMessage {
            version: 70016,
            services: ServiceFlags::WITNESS
                | ServiceFlags::BLOOM
                | ServiceFlags::NETWORK
                | ServiceFlags::COMPACT_FILTERS
                | ServiceFlags::NETWORK_LIMITED,
            timestamp: now() as i64,
            receiver: Address::new(&self.addr, ServiceFlags::NONE),
            // TODO: Fill in our address from discover
            // TODO: Fill in services if we got the address from another peers addr
            sender: Address::new(&"0.0.0.0:0".parse().unwrap(), ServiceFlags::NONE),
            // TODO: Generate random nonce from pool that tracks nonces for all peers to ensure we don't connect to ourselves
            nonce: 0,
            user_agent: "/rust_bitcoin_node:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        };

        self.queue_message(NetworkMessage::Version(version));
    }
}

/// Message Senders
impl Peer {
    pub fn ping(&self, nonce: u64) {
        let mut state = self.state.lock();

        state.ping_nonce_sent = Some(nonce);

        self.queue_message(NetworkMessage::Ping(nonce));
    }

    pub fn get_headers(&self, locator: Vec<BlockHash>, stop: Option<BlockHash>) {
        if locator.is_empty() {
            return;
        }
        debug!(
            "Requesting headers packet from peer with getheaders ({}).",
            self.addr
        );
        debug!("Sending getheaders (hash={}, stop={:?}).", locator[0], stop);
        self.queue_message(NetworkMessage::GetHeaders(GetHeadersMessage::new(
            locator,
            stop.unwrap_or(BlockHash::default()),
        )));
    }
}

/// Message Handlers
impl Peer {
    pub fn handle_message(&self, message: &NetworkMessage) -> Result<()> {
        match message {
            NetworkMessage::Version(_) | NetworkMessage::Verack | NetworkMessage::WtxidRelay => {
                bail!("handshake message after handshake");
            }

            NetworkMessage::SendHeaders => self.handle_send_headers(),

            NetworkMessage::Ping(nonce) => self.handle_ping(*nonce),

            NetworkMessage::Pong(_) => {}

            NetworkMessage::FilterLoad(_) => {}

            NetworkMessage::FilterAdd(_) => {}

            NetworkMessage::FilterClear => {}

            NetworkMessage::SendCmpct(send_cmpct) => self.handle_send_cmpct(send_cmpct.clone()),

            NetworkMessage::FeeFilter(_) => {}

            NetworkMessage::SendAddrV2 => self.handle_send_addr_v2(),

            _ => {}
        }
        Ok(())
    }

    pub fn handle_version(&self, version: VersionMessage) -> Result<()> {
        let mut state = self.state.lock();

        state.version = Some(version.version);
        state.services = version.services;
        state.start_height = version.start_height;
        state.user_agent = version.user_agent;
        state.relay = version.relay;

        if version.version < 70001 {
            bail!("not new enough version");
        }

        if self.outbound {
            if !state.services.has(ServiceFlags::NETWORK) {
                bail!("not full node");
            }

            if !state.services.has(ServiceFlags::WITNESS) {
                bail!("not segwit node");
            }
        }

        if !self.outbound {
            self.queue_version_message();
        }

        if version.version >= WTXID_RELAY_VERSION {
            self.queue_message(NetworkMessage::WtxidRelay);
        }

        self.queue_message(NetworkMessage::Verack);

        self.queue_message(NetworkMessage::SendAddrV2);

        Ok(())
    }

    pub fn handle_send_addr_v2(&self) {
        let mut state = self.state.lock();

        state.addr_relay = AddrRelay::V2;
    }

    pub fn handle_send_headers(&self) {
        let mut state = self.state.lock();

        match &mut state.block_relay {
            BlockRelay::Compact { relay, .. } if *relay == Some(CompactRelay::Inv) => {
                relay.replace(CompactRelay::Headers);
            }
            BlockRelay::Inv => {
                state.block_relay = BlockRelay::Headers;
            }
            _ => {}
        }
    }

    pub fn handle_wtxid_relay(&self) {
        let mut state = self.state.lock();

        state.tx_relay = TxRelay::Wtxid;
    }

    pub fn handle_verack(&self) {
        let mut state = self.state.lock();

        state.handshake = true;

        self.queue_message(NetworkMessage::SendHeaders);

        self.queue_message(NetworkMessage::SendCmpct(SendCmpct {
            send_compact: true,
            version: 2,
        }));
    }

    pub fn handle_send_cmpct(&self, send_cmpct: SendCmpct) {
        let mut state = self.state.lock();

        let witness = send_cmpct.version == 2;
        let high_bandwidth = send_cmpct.send_compact;

        match state.block_relay {
            BlockRelay::Compact { .. } => {}
            BlockRelay::Headers | BlockRelay::Inv if high_bandwidth => {
                state.block_relay = BlockRelay::Compact {
                    witness,
                    relay: None,
                }
            }
            BlockRelay::Headers => {
                state.block_relay = BlockRelay::Compact {
                    witness,
                    relay: Some(CompactRelay::Headers),
                }
            }
            BlockRelay::Inv => {
                state.block_relay = BlockRelay::Compact {
                    witness,
                    relay: Some(CompactRelay::Inv),
                }
            }
        }
    }

    pub fn handle_ping(&self, nonce: u64) {
        self.queue_message(NetworkMessage::Pong(nonce));
    }

    pub fn handle_pong(&self, nonce: u64) {
        let mut state = self.state.lock();

        if matches!(state.ping_nonce_sent, Some(ping_nonce_sent) if ping_nonce_sent == nonce) {
            state.ping_nonce_sent = None;
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn handshake_test() {
        env_logger::builder().format_timestamp_millis().init();

        let stream = TcpStream::connect("hetzner:18333").await.unwrap();
        let addr = stream.peer_addr().unwrap();
        let (peer, mut event_rx, _) = Peer::new(addr, stream, Network::Testnet);

        peer.handshake(&mut event_rx).await.unwrap();
    }

    #[tokio::test]
    async fn force_disconnect() {
        let stream = TcpStream::connect("hetzner:18333").await.unwrap();
        let addr = stream.peer_addr().unwrap();
        let (peer, mut event_rx, mut disconnect_rx) = Peer::new(addr, stream, Network::Testnet);

        peer.handshake(&mut event_rx).await.unwrap();

        #[derive(Debug, thiserror::Error, Eq, PartialEq, Clone)]
        #[error("{0}")]
        struct CustomError(String);

        let error = CustomError("force disconnect".to_string());

        peer.disconnect(Err(error.clone().into()));

        let received_error: CustomError = disconnect_rx
            .try_recv()
            .unwrap()
            .unwrap_err()
            .downcast()
            .unwrap();

        assert_eq!(received_error, error);
    }
}
