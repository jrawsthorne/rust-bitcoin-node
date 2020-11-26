use std::{
    collections::HashMap, collections::HashSet, io::BufWriter, net::IpAddr, net::Ipv4Addr,
    net::Shutdown, net::SocketAddr, net::TcpStream, sync::Arc,
};

use anyhow::{bail, ensure, Result};
use bitcoin::{
    consensus::encode,
    hashes::sha256d,
    network::message_blockdata::GetHeadersMessage,
    network::message_blockdata::Inventory,
    network::message_compact_blocks::SendCmpct,
    network::message_network::VersionMessage,
    network::Address,
    network::{
        constants::ServiceFlags,
        message::{NetworkMessage, RawNetworkMessage},
        message_compact_blocks::CmpctBlock,
        stream_reader::StreamReader,
    },
    util::bip152::HeaderAndShortIds,
    Block, BlockHash, BlockHeader, Network, Txid, Wtxid,
};
use crossbeam_channel::{Receiver, Sender};
use log::{debug, info, trace, warn};
use parking_lot::RwLock;

use crate::{
    protocol::{consensus, WTXID_RELAY_VERSION},
    util::now,
};

use super::{peer_manager::GenericTxid, BloomFilter, CompactBlock, PeerManager};

pub struct Peer {
    manager: Arc<PeerManager>,
    pub state: RwLock<State>,
    pub addr: SocketAddr,
    pub outbound: bool,
    sender: Sender<(NetworkMessage, Option<Sender<()>>)>,
}

pub struct ConnectionReader {
    peer: Arc<Peer>,
    reader: StreamReader<TcpStream>,
    addr: SocketAddr,
}

impl ConnectionReader {
    pub fn new(peer: Arc<Peer>, stream: TcpStream) -> Self {
        Self {
            peer,
            addr: stream.peer_addr().unwrap(),
            reader: StreamReader::new(stream, None),
        }
    }

    pub fn run(&mut self) {
        let mut run = || {
            while let Ok(message) = self.reader.read_next::<RawNetworkMessage>() {
                trace!("received {} from {}", message.cmd(), self.addr);
                self.peer.handle_message(message.payload)?;

                if self.peer.state.read().disconnect {
                    break;
                }
            }
            Result::<()>::Ok(())
        };
        if let Err(err) = run() {
            warn!("error reading from peer: {} ({})", err, self.addr);
        }
        let _ = self.reader.stream.shutdown(Shutdown::Both);
        self.peer.state.write().disconnect = true;
        trace!("connection reader ended ({})", self.addr);
    }
}

pub struct ConnectionWriter {
    rx: Receiver<(NetworkMessage, Option<Sender<()>>)>,
    stream: BufWriter<TcpStream>,
    network: Network,
    addr: SocketAddr,
    peer: Arc<Peer>,
}

impl ConnectionWriter {
    pub fn new(
        rx: Receiver<(NetworkMessage, Option<Sender<()>>)>,
        stream: TcpStream,
        network: Network,
        peer: Arc<Peer>,
    ) -> Self {
        Self {
            rx,
            addr: stream.peer_addr().unwrap(),
            stream: BufWriter::new(stream),
            network,
            peer,
        }
    }

    pub fn run(&mut self) {
        use std::io::Write;

        let mut run = || {
            while let Ok((payload, done)) = self.rx.recv() {
                let raw = &RawNetworkMessage {
                    magic: self.network.magic(),
                    payload,
                };
                self.stream.write_all(&encode::serialize(raw))?;
                self.stream.flush()?;
                trace!("sent {} to {}", raw.cmd(), self.addr);
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
            Result::<()>::Ok(())
        };

        if run().is_err() {
            let _ = self.stream.get_ref().shutdown(Shutdown::Both);
            self.peer.state.write().disconnect = true;
        }

        trace!("connection writer ended ({})", self.addr);
    }
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

pub struct State {
    pub ack: bool,
    pub version: Option<u32>,
    pub services: ServiceFlags,
    pub start_height: i32,
    pub relay: bool,
    pub user_agent: String,
    pub prefer_headers: bool,
    pub spv_filter: Option<BloomFilter>,
    pub fee_rate: u64,
    pub send_compact: Option<bool>,
    pub compact_witness: bool,
    pub inv_filter: HashSet<sha256d::Hash>,
    pub addr_filter: HashSet<Address>,
    pub sent_addr: bool,
    pub tx_map: HashSet<GenericTxid>,
    pub block_map: HashSet<BlockHash>,
    pub best_hash: Option<BlockHash>,
    pub best_height: Option<u32>,
    pub compact_blocks: HashMap<BlockHash, CompactBlock>,
    pub sent_get_addr: bool,
    pub disconnect: bool,
    pub tx_relay: TxRelay,
    pub addr_relay: AddrRelay,
    pub ping_nonce_sent: Option<u64>,
}

impl Peer {
    pub fn new_inbound(
        stream: TcpStream,
        manager: Arc<PeerManager>,
        network: Network,
    ) -> Arc<Self> {
        let addr = stream.peer_addr().unwrap();
        Peer::new(addr, stream, manager, network, false)
    }

    pub fn new_outbound(
        stream: TcpStream,
        manager: Arc<PeerManager>,
        network: Network,
    ) -> Arc<Self> {
        let addr = stream.peer_addr().unwrap();
        Peer::new(addr, stream, manager, network, true)
    }

    fn new(
        addr: SocketAddr,
        stream: TcpStream,
        manager: Arc<PeerManager>,
        network: Network,
        outbound: bool,
    ) -> Arc<Self> {
        let (sender, rx) = crossbeam_channel::unbounded();

        let peer = Arc::new(Peer {
            addr,
            outbound,
            manager,
            state: RwLock::new(State {
                ack: false,
                version: None,
                services: ServiceFlags::NONE,
                start_height: 0,
                relay: false,
                user_agent: "".to_string(),
                prefer_headers: false,
                spv_filter: None,
                fee_rate: 0,
                send_compact: None,
                compact_witness: false,
                inv_filter: HashSet::new(),
                addr_filter: HashSet::new(),
                sent_addr: false,
                tx_map: HashSet::new(),
                block_map: HashSet::new(),
                best_hash: None,
                best_height: None,
                compact_blocks: HashMap::new(),
                sent_get_addr: false,
                disconnect: false,
                addr_relay: AddrRelay::V1,
                tx_relay: TxRelay::Txid,
                ping_nonce_sent: None,
            }),
            sender,
        });

        let mut connection_writer =
            ConnectionWriter::new(rx, stream.try_clone().unwrap(), network, peer.clone());
        let mut connection_reader = ConnectionReader::new(peer.clone(), stream);

        std::thread::spawn(move || connection_writer.run());
        std::thread::spawn(move || connection_reader.run());

        peer
    }

    pub fn send_compact(&self, send_compact: bool) -> Result<()> {
        info!("Initializing witness compact blocks ({}).", self.addr);
        // we only connect to segwit nodes
        self.send(NetworkMessage::SendCmpct(SendCmpct {
            send_compact,
            version: 2,
        }))?;
        self.send(NetworkMessage::SendCmpct(SendCmpct {
            send_compact,
            version: 1,
        }))
    }

    pub fn send_prefer_headers(&self) -> Result<()> {
        info!("Initializing headers-first sync ({}).", self.addr);
        self.send(NetworkMessage::SendHeaders)
    }

    pub fn send_mempool(&self) -> Result<()> {
        if !self.state.read().services.has(ServiceFlags::BLOOM) {
            return Ok(());
        }

        debug!(
            "Requesting inv packet from peer with mempool ({}).",
            self.addr
        );

        self.send(NetworkMessage::MemPool)
    }

    pub fn send_get_headers(
        &self,
        locator_hashes: Vec<BlockHash>,
        stop_hash: Option<BlockHash>,
    ) -> Result<()> {
        if locator_hashes.is_empty() {
            return Ok(());
        }

        let start_hash = locator_hashes[0];
        let stop_hash = stop_hash.unwrap_or_default();

        let message = GetHeadersMessage::new(locator_hashes, stop_hash);

        debug!(
            "Requesting headers packet from peer with getheaders ({}).",
            self.addr
        );

        debug!(
            "Sending getheaders (hash={}, stop={}).",
            start_hash, stop_hash
        );

        self.send_and_wait(NetworkMessage::GetHeaders(message))
    }

    pub fn send_get_addr(&self) -> Result<()> {
        let mut state = self.state.write();
        if state.sent_get_addr {
            return Ok(());
        }

        state.sent_get_addr = true;

        self.send(NetworkMessage::GetAddr)
    }

    pub fn announce_block(&self, blocks: Vec<Block>) -> Result<()> {
        let mut state = self.state.write();

        let mut headers = vec![];
        let mut inv = vec![];

        for block in blocks {
            if state.inv_filter.contains(&block.block_hash().as_hash()) {
                continue;
            }

            if state.send_compact.unwrap_or(false) {
                state.inv_filter.insert(block.block_hash().as_hash());
                self.send_compact_block(&block)?;
                continue;
            }

            if state.prefer_headers {
                headers.push(block.header);
                continue;
            }

            inv.push(Inventory::Block(block.block_hash()));
        }

        if state.prefer_headers {
            drop(state);
            return self.send_headers(headers);
        }

        self.queue_inv(inv);

        Ok(())
    }

    pub fn send_fee_rate(&self, rate: u64) -> Result<()> {
        self.send(NetworkMessage::FeeFilter(rate as i64))
    }

    pub fn get_data(&self, items: Vec<Inventory>) -> Result<()> {
        self.send(NetworkMessage::GetData(items))
    }

    pub fn get_transactions(&self, gtxids: Vec<GenericTxid>) -> Result<()> {
        self.get_data(
            gtxids
                .into_iter()
                .map(|gtxid| {
                    if gtxid.wtxid {
                        Inventory::WTx(Wtxid::from_hash(gtxid.hash))
                    } else {
                        Inventory::WitnessTransaction(Txid::from_hash(gtxid.hash))
                    }
                })
                .collect(),
        )
    }

    fn block_inv(&self, hash: BlockHash) -> Inventory {
        if self.state.read().send_compact.unwrap_or(false) {
            Inventory::CompactBlock(hash)
        } else {
            Inventory::WitnessBlock(hash)
        }
    }

    pub fn get_blocks(&self, hashes: Vec<BlockHash>) -> Result<()> {
        self.get_data(
            hashes
                .into_iter()
                .map(|hash| self.block_inv(hash))
                .collect(),
        )
    }

    pub fn get_full_block(&self, hash: BlockHash) -> Result<()> {
        self.get_data(vec![Inventory::WitnessBlock(hash)])
    }

    pub fn send_headers(&self, headers: Vec<BlockHeader>) -> Result<()> {
        if headers.is_empty() {
            return Ok(());
        }

        let mut state = self.state.write();

        for header in &headers {
            state.inv_filter.insert(header.block_hash().as_hash());
        }

        trace!("Serving {} headers to {}.", headers.len(), self.addr);

        let mut headers = headers;
        loop {
            if headers.len() > 2000 {
                let a = headers.split_off(2000);
                self.send(NetworkMessage::Headers(headers))?;
                headers = a;
            } else {
                self.send(NetworkMessage::Headers(headers))?;
                break Ok(());
            }
        }
    }

    pub fn queue_inv(&self, _inv: Vec<Inventory>) {}

    pub fn send_compact_block(&self, block: &Block) -> Result<()> {
        use bitcoin::secp256k1::rand::random;

        let compact_block = HeaderAndShortIds::from_block(block, random(), 2, &[]).unwrap();

        self.send(NetworkMessage::CmpctBlock(CmpctBlock { compact_block }))
    }

    pub fn handle_ping(&self, nonce: u64) -> Result<()> {
        self.send(NetworkMessage::Pong(nonce))
    }

    pub fn handle_verack(&self) {
        let mut state = self.state.write();
        if state.ack {
            debug!("Peer sent duplicate ack ({}).", self.addr);
            return;
        }

        state.ack = true;
        debug!("Received verack ({}).", self.addr);
    }

    pub fn handle_version(&self, version: VersionMessage) -> Result<()> {
        let mut state = self.state.write();

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
            // TODO: Check for compact blocks
        }

        if !self.outbound {
            self.send_version()?;
        }

        if version.version >= WTXID_RELAY_VERSION {
            self.send(NetworkMessage::WtxidRelay)?;
        }

        self.send(NetworkMessage::Verack)?;

        self.send(NetworkMessage::SendAddrV2)
    }

    pub fn handle_send_headers(&self) {
        let mut state = self.state.write();

        if state.prefer_headers {
            debug!("Peer sent a duplicate sendheaders ({}).", self.addr);
            return;
        }

        state.prefer_headers = true;
    }

    pub fn handle_pong(&self, nonce: u64) {
        let mut state = self.state.write();

        if matches!(state.ping_nonce_sent, Some(ping_nonce_sent) if ping_nonce_sent == nonce) {
            state.ping_nonce_sent = None;
        }
    }

    pub fn send_ping(&self, nonce: u64) -> Result<()> {
        let mut state = self.state.write();

        state.ping_nonce_sent = Some(nonce);

        self.send(NetworkMessage::Ping(nonce))
    }

    pub fn handle_filter_load(&self, filter: BloomFilter) -> Result<()> {
        if !filter.is_within_size_constraints() {
            bail!("filter not within constraints");
        }

        let mut state = self.state.write();
        state.spv_filter = Some(filter);
        state.relay = true;

        Ok(())
    }

    pub fn handle_filter_add(&self, data: &[u8]) -> Result<()> {
        if data.len() > consensus::MAX_SCRIPT_PUSH {
            bail!("filter too big");
        }

        let mut state = self.state.write();

        if let Some(ref mut filter) = state.spv_filter {
            filter.insert(data);
        }

        state.relay = true;

        Ok(())
    }

    pub fn handle_filter_clear(&self) {
        let mut state = self.state.write();

        state.spv_filter = None;
        state.relay = true;
    }

    pub fn handle_fee_filter(&self, rate: i64) {
        let mut state = self.state.write();

        if rate >= 0 && rate as u64 <= consensus::MAX_MONEY && state.relay {
            state.fee_rate = rate as u64;
        }
    }

    pub fn handle_send_cmpct(&self, sendcmpct: SendCmpct) {
        let version = sendcmpct.version;
        let send_compact = sendcmpct.send_compact;

        let mut state = self.state.write();

        if state.send_compact.is_some() {
            return;
        }

        state.send_compact = Some(send_compact);
        state.compact_witness = version == 2;
    }

    pub fn handle_message(self: &Arc<Self>, message: NetworkMessage) -> Result<()> {
        match &message {
            NetworkMessage::Version(version) => self.handle_version(version.clone())?,
            NetworkMessage::Verack => self.handle_verack(),
            NetworkMessage::WtxidRelay => {
                let mut state = self.state.write();
                if state.version.is_none() || state.ack {
                    bail!("wtxidrelay must be sent between version and verack");
                } else {
                    info!("peer wants transactions relayed by wtxid ({})", self.addr);
                    state.tx_relay = TxRelay::Wtxid;
                }
            }
            _ => {
                ensure!(
                    self.state.read().ack,
                    "non handshake message before handshake complete"
                );
                match &message {
                    NetworkMessage::SendHeaders => self.handle_send_headers(),
                    NetworkMessage::Ping(nonce) => self.handle_ping(*nonce)?,
                    NetworkMessage::Pong(nonce) => self.handle_pong(*nonce),
                    NetworkMessage::FeeFilter(rate) => self.handle_fee_filter(*rate),
                    NetworkMessage::FilterLoad(filter) => {
                        self.handle_filter_load(filter.clone().into())?
                    }
                    NetworkMessage::FilterAdd(filter) => self.handle_filter_add(&filter.data)?,
                    NetworkMessage::FilterClear => self.handle_filter_clear(),
                    NetworkMessage::SendCmpct(sendcmpct) => {
                        self.handle_send_cmpct(sendcmpct.clone())
                    }
                    NetworkMessage::SendAddrV2 => {
                        info!("peer wants addrv2 ({})", self.addr);
                        self.state.write().addr_relay = AddrRelay::V2;
                    }
                    _ => {}
                }
            }
        }

        self.manager.handle_message(self, message)
    }

    pub fn send_version(&self) -> Result<()> {
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
            sender: Address::new(
                &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
                ServiceFlags::NONE,
            ),
            // TODO: Generate random nonce from pool that tracks nonces for all peers to ensure we don't connect to ourselves
            nonce: 0,
            user_agent: "/rust_bitcoin_node:0.1.0/".to_string(),
            start_height: self.manager.chain.read().height as i32,
            relay: true,
        };
        self.send(NetworkMessage::Version(version))
    }

    pub fn send(&self, message: NetworkMessage) -> Result<()> {
        self.sender.send((message, None))?;
        Ok(())
    }

    pub fn send_and_wait(&self, message: NetworkMessage) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(0);
        self.sender.send((message, Some(tx)))?;
        rx.recv()?;
        Ok(())
    }
}

pub type PeerRef<'a> = &'a Arc<Peer>;
