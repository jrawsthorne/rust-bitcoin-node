use super::{BloomFilter, CompactBlock};
use crate::{
    blockchain::Chain, indexers::FilterIndexer, mempool::MemPool, protocol::consensus, util::now,
};
use bitcoin::network::constants::PROTOCOL_VERSION;
use bitcoin::{
    hashes::sha256d,
    network::{
        constants::ServiceFlags,
        message::{NetworkMessage, RawNetworkMessage},
        message_blockdata::{GetHeadersMessage, Inventory},
        message_compact_blocks::{BlockTxn, CmpctBlock, GetBlockTxn, SendCmpct},
        message_filter::{CFHeaders, CFilter, GetCFHeaders, GetCFilters},
        message_network::VersionMessage,
        stream_reader::StreamReader,
        Address,
    },
    util::bip152::{BlockTransactions, BlockTransactionsRequest, HeaderAndShortIds},
    Block, BlockHash, BlockHeader, MerkleBlock, Network, Transaction, Txid,
};
use crossbeam_channel::{Receiver, Sender};
use log::{debug, info, trace, warn};
use parking_lot::RwLock;
use std::net::{IpAddr, Ipv4Addr};
use std::{
    collections::{HashMap, HashSet},
    io::BufWriter,
    net::{SocketAddr, TcpStream},
    sync::Arc,
};

pub struct Peer {
    pool: Arc<Pool>,
    state: RwLock<State>,
    pub addr: SocketAddr,
    outbound: bool,
    sender: Sender<(NetworkMessage, Option<Sender<()>>)>,
}

pub trait MessageHandler {
    fn handle_message(&self, message: NetworkMessage);
}

pub struct ConnectionReader<H> {
    handler: H,
    reader: StreamReader<TcpStream>,
    addr: SocketAddr,
}

impl<H: MessageHandler> ConnectionReader<H> {
    pub fn new(handler: H, stream: TcpStream) -> Self {
        Self {
            handler,
            addr: stream.peer_addr().unwrap(),
            reader: StreamReader::new(stream, None),
        }
    }

    pub fn run(&mut self) {
        while let Ok(message) = self.reader.read_next::<RawNetworkMessage>() {
            trace!("received {} from {}", message.cmd(), self.addr);
            self.handler.handle_message(message.payload);
        }
    }
}

pub struct ConnectionWriter {
    rx: Receiver<(NetworkMessage, Option<Sender<()>>)>,
    tx: Sender<(NetworkMessage, Option<Sender<()>>)>,
    stream: BufWriter<TcpStream>,
    network: Network,
    buf: Vec<u8>, // reduce allocations
    addr: SocketAddr,
}

impl ConnectionWriter {
    pub fn new(stream: TcpStream, network: Network) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            tx,
            rx,
            addr: stream.peer_addr().unwrap(),
            stream: BufWriter::new(stream),
            network,
            buf: Vec::with_capacity(16 * 1024),
        }
    }

    pub fn sender(&self) -> Sender<(NetworkMessage, Option<Sender<()>>)> {
        self.tx.clone()
    }

    pub fn run(&mut self) {
        use bitcoin::consensus::Encodable;
        use std::io::Write;

        while let Ok((payload, done)) = self.rx.recv() {
            let raw = RawNetworkMessage {
                magic: self.network.magic(),
                payload,
            };
            raw.consensus_encode(&mut self.buf).unwrap();
            self.stream.write_all(&self.buf).unwrap();
            self.stream.flush().unwrap();
            self.buf.clear();
            trace!("sent {} to {}", raw.cmd(), self.addr);
            if let Some(done) = done {
                done.send(()).unwrap();
            }
        }
    }
}

impl MessageHandler for Arc<Peer> {
    fn handle_message(&self, message: NetworkMessage) {
        self.handle_message(message);
    }
}

struct State {
    ack: bool,
    version: Option<u32>,
    services: ServiceFlags,
    start_height: i32,
    relay: bool,
    user_agent: String,
    prefer_headers: bool,
    spv_filter: Option<BloomFilter>,
    fee_rate: u64,
    send_compact: Option<bool>,
    compact_witness: bool,
    inv_filter: HashSet<sha256d::Hash>,
    addr_filter: HashSet<Address>,
    sent_addr: bool,
    tx_map: HashSet<Txid>,
    block_map: HashSet<BlockHash>,
    best_hash: Option<BlockHash>,
    best_height: Option<u32>,
    compact_blocks: HashMap<BlockHash, CompactBlock>,
    sent_get_addr: bool,
}

impl Peer {
    pub fn new_inbound(stream: TcpStream, pool: Arc<Pool>, network: Network) -> Arc<Self> {
        let addr = stream.peer_addr().unwrap();
        Peer::new(addr, stream, pool, network, false)
    }

    pub fn new_outbound(addr: SocketAddr, pool: Arc<Pool>, network: Network) -> Arc<Self> {
        let stream = TcpStream::connect(addr).unwrap();
        Peer::new(addr, stream, pool, network, true)
    }

    fn new(
        addr: SocketAddr,
        stream: TcpStream,
        pool: Arc<Pool>,
        network: Network,
        outbound: bool,
    ) -> Arc<Self> {
        let mut connection_writer = ConnectionWriter::new(stream.try_clone().unwrap(), network);
        let sender = connection_writer.sender();

        let peer = Arc::new(Peer {
            addr,
            outbound,
            pool,
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
            }),
            sender,
        });

        let mut connection_reader = ConnectionReader::new(peer.clone(), stream);

        std::thread::spawn(move || connection_writer.run());
        std::thread::spawn(move || connection_reader.run());

        peer
    }

    pub fn send_compact(&self, send_compact: bool) {
        info!("Initializing witness compact blocks ({}).", self.addr);
        self.send(NetworkMessage::SendCmpct(SendCmpct {
            send_compact,
            version: 2,
        }));
    }

    pub fn send_prefer_headers(&self) {
        info!("Initializing headers-first sync ({}).", self.addr);
        self.send(NetworkMessage::SendHeaders);
    }

    pub fn send_mempool(&self) {
        if !self.state.read().services.has(ServiceFlags::BLOOM) {
            return;
        }

        debug!(
            "Requesting inv packet from peer with mempool ({}).",
            self.addr
        );

        self.send(NetworkMessage::MemPool);
    }

    pub fn send_get_headers(&self, locator_hashes: Vec<BlockHash>, stop_hash: Option<BlockHash>) {
        if locator_hashes.is_empty() {
            return;
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

        self.send_and_wait(NetworkMessage::GetHeaders(message));
    }

    pub fn send_get_addr(&self) {
        let mut state = self.state.write();
        if state.sent_get_addr {
            return;
        }

        state.sent_get_addr = true;

        self.send(NetworkMessage::GetAddr);
    }

    pub fn announce_block(&self, blocks: Vec<Block>) {
        let mut state = self.state.write();

        let mut headers = vec![];
        let mut inv = vec![];

        for block in blocks {
            if state.inv_filter.contains(&block.block_hash().as_hash()) {
                continue;
            }

            if state.send_compact.unwrap_or(false) {
                state.inv_filter.insert(block.block_hash().as_hash());
                self.send_compact_block(&block);
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
            self.send_headers(headers);
            return;
        }

        self.queue_inv(inv);
    }

    pub fn send_fee_rate(&self, rate: u64) {
        self.send(NetworkMessage::FeeFilter(rate as i64));
    }

    pub fn get_data(&self, items: Vec<Inventory>) {
        self.send(NetworkMessage::GetData(items));
    }

    pub fn get_transactions(&self, txids: Vec<Txid>) {
        self.get_data(
            txids
                .into_iter()
                .map(|txid| Inventory::WitnessTransaction(txid))
                .collect(),
        );
    }

    fn block_inv(&self, hash: BlockHash) -> Inventory {
        if self.state.read().send_compact.unwrap_or(false) {
            Inventory::CompactBlock(hash)
        } else {
            Inventory::WitnessBlock(hash)
        }
    }

    pub fn get_blocks(&self, hashes: Vec<BlockHash>) {
        self.get_data(
            hashes
                .into_iter()
                .map(|hash| self.block_inv(hash))
                .collect(),
        );
    }

    pub fn get_full_block(&self, hash: BlockHash) {
        self.get_data(vec![Inventory::WitnessBlock(hash)]);
    }

    pub fn send_headers(&self, headers: Vec<BlockHeader>) {
        if headers.is_empty() {
            return;
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
                self.send(NetworkMessage::Headers(headers));
                headers = a;
            } else {
                self.send(NetworkMessage::Headers(headers));
                break;
            }
        }
    }

    pub fn queue_inv(&self, inv: Vec<Inventory>) {}

    pub fn send_compact_block(&self, block: &Block) {
        use bitcoin::secp256k1::rand::random;

        let compact_block = HeaderAndShortIds::from_block(block, random(), 2, &[]).unwrap();

        self.send(NetworkMessage::CmpctBlock(CmpctBlock { compact_block }));
    }

    pub fn handle_ping(&self, nonce: u64) {
        self.send(NetworkMessage::Pong(nonce));
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

    pub fn handle_version(&self, version: VersionMessage) {
        let mut state = self.state.write();

        state.version = Some(version.version);
        state.services = version.services;
        state.start_height = version.start_height;
        state.user_agent = version.user_agent;
        state.relay = version.relay;

        if version.version < 70001 {
            panic!("not new enough version");
        }

        if self.outbound {
            if !state.services.has(ServiceFlags::NETWORK) {
                panic!("not full node");
            }
            if !state.services.has(ServiceFlags::WITNESS) {
                panic!("not segwit node");
            }
            // TODO: Check for compact blocks
        }

        if !self.outbound {
            self.send_version();
        }

        self.send(NetworkMessage::Verack);
    }

    pub fn handle_send_headers(&self) {
        let mut state = self.state.write();

        if state.prefer_headers {
            debug!("Peer sent a duplicate sendheaders ({}).", self.addr);
            return;
        }

        state.prefer_headers = true;
    }

    pub fn handle_pong(&self, nonce: u64) {}

    pub fn handle_filter_load(&self, filter: BloomFilter) {
        if !filter.is_within_size_constraints() {
            // TODO: Ban
            return;
        }

        let mut state = self.state.write();
        state.spv_filter = Some(filter);
        state.relay = true;
    }

    pub fn handle_filter_add(&self, data: &[u8]) {
        if data.len() > consensus::MAX_SCRIPT_PUSH {
            // TODO: Ban
            return;
        }

        let mut state = self.state.write();

        if let Some(ref mut filter) = state.spv_filter {
            filter.insert(data);
        }

        state.relay = true;
    }

    pub fn handle_filter_clear(&self) {
        let mut state = self.state.write();

        state.spv_filter = None;
        state.relay = true;
    }

    pub fn handle_fee_filter(&self, rate: i64) {
        if rate < 0 || rate as u64 > consensus::MAX_MONEY {
            // TODO: Ban
            return;
        }

        self.state.write().fee_rate = rate as u64;
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

    pub fn handle_message(self: &Arc<Self>, message: NetworkMessage) {
        match &message {
            NetworkMessage::Version(version) => self.handle_version(version.clone()),
            NetworkMessage::Verack => self.handle_verack(),
            NetworkMessage::SendHeaders => self.handle_send_headers(),
            NetworkMessage::Ping(nonce) => self.handle_ping(*nonce),
            NetworkMessage::Pong(nonce) => self.handle_pong(*nonce),
            NetworkMessage::FeeFilter(rate) => self.handle_fee_filter(*rate),
            NetworkMessage::FilterLoad(filter) => self.handle_filter_load(filter.clone().into()),
            NetworkMessage::FilterAdd(filter) => self.handle_filter_add(&filter.data),
            NetworkMessage::FilterClear => self.handle_filter_clear(),
            NetworkMessage::SendCmpct(sendcmpct) => self.handle_send_cmpct(sendcmpct.clone()),
            _ => {}
        }
        self.pool.handle_message(self, message);
    }

    pub fn send_version(&self) {
        let version = VersionMessage {
            version: PROTOCOL_VERSION,
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
            start_height: self.pool.chain.read().height as i32,
            relay: true,
        };
        self.send(NetworkMessage::Version(version));
    }

    pub fn send(&self, message: NetworkMessage) {
        self.sender.send((message, None)).unwrap();
    }

    pub fn send_and_wait(&self, message: NetworkMessage) {
        let (tx, rx) = crossbeam_channel::bounded(0);
        self.sender.send((message, Some(tx))).unwrap();
        rx.recv().unwrap();
    }
}

pub type PeerRef<'a> = &'a Arc<Peer>;

fn required_services() -> ServiceFlags {
    ServiceFlags::WITNESS | ServiceFlags::NETWORK
}

pub struct Pool {
    pub state: RwLock<PoolState>,
    chain: Arc<RwLock<Chain>>,
    mempool: Option<Arc<RwLock<MemPool>>>,
    filter_index: Option<Arc<RwLock<FilterIndexer>>>,
}

#[derive(Default)]
pub struct PoolState {
    pub peers: HashMap<SocketAddr, Arc<Peer>>,
    tx_map: HashSet<Txid>,
    block_map: HashSet<BlockHash>,
    verifying: HashSet<sha256d::Hash>,
    compact_blocks: HashSet<BlockHash>,
}

impl Pool {
    pub fn new(
        chain: Arc<RwLock<Chain>>,
        mempool: Option<Arc<RwLock<MemPool>>>,
        filter_index: Option<Arc<RwLock<FilterIndexer>>>,
    ) -> Arc<Pool> {
        Arc::new(Pool {
            state: Default::default(),
            chain,
            mempool,
            filter_index,
        })
    }

    pub fn handle_version(&self, peer: PeerRef, version: VersionMessage) {
        info!(
            "Received version ({}): version={} height={} services={} agent={}",
            peer.addr, version.version, version.start_height, version.services, version.user_agent
        );

        // TODO: Mark as good peer
    }

    pub fn handle_get_addr(&self, peer: PeerRef) {
        let mut state = peer.state.write();

        if state.sent_addr {
            debug!("Ignoring repeated getaddr ({}).", peer.addr);
        }

        state.sent_addr = true;

        // TODO: Send list of seen addresses
    }

    pub fn handle_addr(&self, peer: PeerRef, addrs: Vec<(u32, Address)>) {
        let mut state = peer.state.write();

        let services = required_services();
        let now = now() as u32;

        let count = addrs.len();

        for (mut time, addr) in addrs {
            state.addr_filter.insert(addr.clone());

            if !addr.services.has(services) {
                continue;
            }

            if time <= 100000000 || time > now + 10 * 60 {
                time = now - 5 * 24 * 60 * 60;
            }

            if addr.port == 0 {
                continue;
            }

            // TODO: Add to seen addresses
        }

        info!(
            "Received {} addrs (hosts={}, peers={}) ({}).",
            count,
            0,
            self.state.read().peers.len(),
            peer.addr
        );
    }

    pub fn handle_inv(&self, peer: PeerRef, items: Vec<Inventory>) {
        let mut txids = vec![];

        let mut peer_state = peer.state.write();
        let mut state = self.state.write();

        for item in items {
            if let Inventory::Transaction(txid) = item {
                if state.tx_map.contains(&txid) {
                    continue;
                }

                peer_state.inv_filter.insert(txid.as_hash());
                state.tx_map.insert(txid);
                peer_state.tx_map.insert(txid);

                txids.push(txid);
            }
        }

        if !txids.is_empty() {
            debug!(
                "Requesting {}/{} txs from peer with getdata ({}).",
                txids.len(),
                state.tx_map.len(),
                peer.addr
            );
            peer.get_transactions(txids);
        }
    }

    pub fn has_tx(&self, txid: &Txid) -> bool {
        if self.state.read().verifying.contains(&txid[..]) {
            return true;
        }

        if let Some(mempool) = &self.mempool {
            if mempool.read().transactions.contains_key(txid) {
                return true;
            }
        }

        false
    }

    pub fn handle_get_data(&self, peer: PeerRef, items: Vec<Inventory>) {
        if items.len() > 50_000 {
            // TODO: Ban
            return;
        }

        let mut txs = 0;
        let mut blocks = 0;
        let mut compact = 0;
        let mut not_found = vec![];

        // TODO: Unknown

        for item in items {
            match item {
                Inventory::WitnessTransaction(txid) => {
                    let tx = self.mempool.as_ref().and_then(|mempool| {
                        mempool
                            .read()
                            .transactions
                            .get(&txid)
                            .map(|entry| entry.tx.clone())
                    });

                    if let Some(tx) = tx {
                        if tx.is_coin_base() {
                            not_found.push(item);
                            warn!("Failsafe: tried to relay a coinbase.");
                            continue;
                        }

                        peer.send(NetworkMessage::Tx(tx));

                        txs += 1;
                    } else {
                        not_found.push(item);
                    }
                }
                Inventory::WitnessBlock(hash) => {
                    let block = self.chain.read().db.get_block(hash).unwrap();
                    if let Some(block) = block {
                        peer.send(NetworkMessage::Block(block));
                        blocks += 1;
                    } else {
                        not_found.push(item);
                    }
                }
                Inventory::CompactBlock(hash) => {
                    let chain = self.chain.read();
                    let height = chain.db.get_entry_by_hash(&hash).map(|e| e.height);
                    if let Some(height) = height {
                        // Fallback to full block.
                        if height < chain.tip.height.saturating_sub(10) {
                            let block = self.chain.read().db.get_block(hash).unwrap();
                            if let Some(block) = block {
                                peer.send(NetworkMessage::Block(block));
                                blocks += 1;
                            } else {
                                not_found.push(item);
                            }
                        } else {
                            let block = self.chain.read().db.get_block(hash).unwrap();
                            if let Some(block) = block {
                                peer.send_compact_block(&block);

                                blocks += 1;
                                compact += 1;
                            } else {
                                not_found.push(item);
                            }
                        }
                    } else {
                        not_found.push(item);
                    }
                }
                Inventory::WitnessFilteredBlock(hash) => {
                    let peer_state = peer.state.read();
                    let spv_filter = match &peer_state.spv_filter {
                        None => {
                            not_found.push(item);
                            continue;
                        }
                        Some(spv_filter) => spv_filter,
                    };
                    let block = self.chain.read().db.get_block(hash).unwrap();
                    if let Some(block) = block {
                        let matches: Vec<_> = block
                            .txdata
                            .iter()
                            .filter_map(|tx| {
                                let txid = tx.txid();
                                if spv_filter.contains(&txid) {
                                    Some(tx.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        let match_txids = matches.iter().map(|tx| tx.txid()).collect();
                        let merkle_block = MerkleBlock::from_block(&block, &match_txids);

                        peer.send(NetworkMessage::MerkleBlock(merkle_block));

                        for tx in matches {
                            peer.send(NetworkMessage::Tx(tx));
                            txs += 1;
                        }
                    } else {
                        not_found.push(item);
                    }
                }
                Inventory::Error => {}
                Inventory::Transaction(_) => {}
                Inventory::FilteredBlock(_) => {}
                Inventory::Block(_) => {}
            }
        }

        let not_found_count = not_found.len();

        if !not_found.is_empty() {
            peer.send(NetworkMessage::NotFound(not_found));
        }

        if txs > 0 {
            debug!(
                "Served {} txs with getdata (notfound={}) ({}).",
                txs, not_found_count, peer.addr
            );
        }

        if blocks > 0 {
            debug!(
                "Served {} blocks with getdata (notfound={}, cmpct={}) ({}).",
                blocks, not_found_count, compact, peer.addr
            );
        }
    }

    pub fn handle_mempool(&self, peer: PeerRef) {
        if let Some(mempool) = &self.mempool {
            let items = mempool
                .read()
                .transactions
                .keys()
                .map(|txid| Inventory::WitnessTransaction(*txid))
                .collect();
            peer.send(NetworkMessage::Inv(items));
        }
    }

    pub fn handle_tx(&self, peer: PeerRef, tx: Transaction) {
        let txid = tx.txid();
        let mut state = self.state.write();
        let mut peer_state = peer.state.write();

        if !peer_state.tx_map.remove(&txid) {
            return;
        }
        state.tx_map.remove(&txid);

        if let Some(mempool) = &self.mempool {
            if let Err(err) = mempool.write().add_tx(&self.chain.read(), tx) {
                warn!("{} couldn't be added to mempool: {}", txid, err);
            }
        }
    }

    pub fn handle_block(&self, peer: PeerRef, block: Block) {
        let hash = block.block_hash();

        {
            let mut state = self.state.write();
            let mut peer_state = peer.state.write();

            if !peer_state.block_map.remove(&hash) {
                // TODO: Ban
                return;
            }

            assert!(state.block_map.remove(&hash));
            state.verifying.insert(hash.as_hash());
        }

        self.chain.write().add(block).unwrap();

        assert!(self.state.write().verifying.remove(&hash.as_hash()));

        self.get_next_blocks(peer);
    }

    pub fn get_next_blocks(&self, peer: PeerRef) {
        let chain = self.chain.read();
        let mut state = self.state.write();
        let mut peer_state = peer.state.write();

        if chain.is_recent() && state.block_map.is_empty() {
            let mut height = chain.tip.height;

            let mut hashes = vec![];

            while hashes.is_empty() && height <= chain.most_work().height {
                let next = chain
                    .db
                    .get_next_path(chain.most_work(), height, 16)
                    .into_iter()
                    .filter_map(|e| {
                        if state.verifying.contains(&e.hash.as_hash())
                            || chain.db.has_block(e.hash).unwrap()
                        {
                            None
                        } else {
                            Some(e.hash)
                        }
                    });
                hashes.extend(next);
                height += 16;
            }

            for hash in &hashes {
                state.block_map.insert(*hash);
                peer_state.block_map.insert(*hash);
            }

            drop(state);
            drop(peer_state);
            drop(chain);
            peer.get_blocks(hashes);
        }
    }

    pub fn handle_get_headers(
        &self,
        peer: PeerRef,
        locator_hashes: Vec<BlockHash>,
        stop_hash: BlockHash,
    ) {
        let chain = self.chain.read();

        if !chain.synced() {
            return;
        }

        let hash = if locator_hashes.is_empty() {
            Some(stop_hash)
        } else {
            chain
                .db
                .get_next_hash(chain.find_locator(&locator_hashes))
                .unwrap()
        };

        let mut entry = match hash.and_then(|h| chain.db.get_entry_by_hash(&h)) {
            Some(entry) => entry,
            None => return,
        };

        let mut headers = vec![];

        loop {
            headers.push(entry.into());

            if entry.hash == stop_hash {
                break;
            }

            if headers.len() == 2000 {
                break;
            }

            match chain.db.get_next(entry).unwrap() {
                None => break,
                Some(next) => entry = next,
            }
        }

        peer.send_headers(headers);
    }

    pub fn handle_headers(&self, peer: PeerRef, headers: Vec<BlockHeader>) {
        if headers.is_empty() {
            return;
        }

        let mut last: Option<&BlockHeader> = None;
        for header in &headers {
            if let Some(l) = last {
                if header.prev_blockhash != l.block_hash() {
                    warn!("Peer sent a bad header chain ({}).", peer.addr);
                    // TODO: Ban - bad header chain
                    return;
                }
            }
            last = Some(header);
        }

        let mut chain = self.chain.write();
        let mut peer_state = peer.state.write();

        for header in &headers {
            let hash = header.block_hash();

            peer_state.inv_filter.insert(hash.as_hash());

            if chain.db.has_header(&hash) {
                trace!("Already have header: hash={} ({}).", hash, peer.addr);
                peer_state.best_hash = Some(hash);
                continue;
            }

            match chain.add_header(header) {
                Err(err) => {
                    warn!("{}", err);
                    return;
                }
                Ok((entry, _)) => {
                    peer_state.best_hash = Some(hash);
                    peer_state.best_height = Some(entry.height);
                }
            }
        }

        debug!(
            "Received {} headers from peer ({}).",
            headers.len(),
            peer.addr
        );

        if headers.len() == 2000 {
            let locator_hashes = chain.get_locator(Some(headers[headers.len() - 1].block_hash()));
            peer.send_get_headers(locator_hashes, None);
        }

        drop(peer_state);
        drop(chain);

        self.get_next_blocks(peer);
    }

    pub fn handle_compact_block(&self, peer: PeerRef, header_and_short_ids: HeaderAndShortIds) {
        let mut peer_state = peer.state.write();
        let mut state = self.state.write();

        let header = &header_and_short_ids.header;
        let hash = header.block_hash();

        if peer_state.compact_blocks.contains_key(&hash) {
            debug!("Peer sent us a duplicate compact block ({}).", peer.addr);
            return;
        }

        if state.compact_blocks.contains(&hash) {
            debug!(
                "Already waiting for compact block {} ({}).",
                hash, peer.addr
            );
            return;
        }

        if !peer_state.block_map.contains(&hash) {
            peer_state.block_map.insert(hash);
            assert!(!state.block_map.contains(&hash));
            state.block_map.insert(hash);
        }

        if self.chain.write().add_header(&header).is_err() {
            debug!("Peer sent an invalid compact block ({}).", peer.addr);
            // TODO: Ban
            return;
        }

        let mut block = match CompactBlock::new(header_and_short_ids) {
            None => {
                warn!(
                    "Siphash collision for {}. Requesting full block ({}).",
                    hash, peer.addr
                );
                peer.get_full_block(hash);
                // TODO: Increase ban score
                return;
            }
            Some(block) => block,
        };

        let full = self
            .mempool
            .as_ref()
            .map(|mempool| block.fill_mempool(mempool.read().transactions.values().map(|e| &e.tx)))
            .unwrap_or(false);

        if full {
            drop(peer_state);
            drop(state);
            debug!("Received full compact block {}", hash);
            self.handle_block(peer, block.into());
            return;
        }

        if peer_state.compact_blocks.len() >= 15 {
            warn!("Compact block DoS attempt ({}).", peer.addr);
            // TODO: Ban
            return;
        }

        debug!(
            "Received non-full compact block {} tx={}/{} ({}).",
            hash, block.count, block.total_tx, peer.addr
        );

        let txs_request = block.into_block_transactions_request();

        assert!(peer_state.compact_blocks.insert(hash, block).is_none());
        state.compact_blocks.insert(hash);

        peer.send(NetworkMessage::GetBlockTxn(GetBlockTxn { txs_request }));
    }

    pub fn handle_block_txn(&self, peer: PeerRef, transactions: Vec<Transaction>, hash: BlockHash) {
        let mut state = self.state.write();
        let mut peer_state = peer.state.write();

        let mut block = match peer_state.compact_blocks.remove(&hash) {
            None => {
                debug!("Peer sent unsolicited blocktxn ({}).", peer.addr);
                return;
            }
            Some(block) => block,
        };

        assert!(state.compact_blocks.remove(&hash));

        if !block.fill_missing(transactions) {
            warn!(
                "Peer sent non-full blocktxn for {}. Requesting full block ({}).",
                hash, peer.addr
            );
            peer.get_full_block(hash);
            // TODO: Increase ban score
            return;
        }

        debug!("Filled compact block {} ({})", hash, peer.addr);

        drop(state);
        drop(peer_state);

        self.handle_block(peer, block.into());
    }

    // Kicks everything off for this peer
    pub fn handle_verack(&self, peer: PeerRef) {
        let chain = self.chain.read();

        peer.send_prefer_headers();
        peer.send_compact(true);
        peer.send_get_addr();

        // TODO: Announce

        // TODO: Set new fee rate when synced

        // if !chain.synced() {
        //     peer.send_fee_rate(MAX_MONEY);
        // }

        // if chain.synced() {
        //     peer.send_mempool();
        // }

        if peer.outbound {
            let best = chain.most_work();
            let hash = chain.db.get_entry_by_hash(&best.prev_block).map(|e| e.hash);
            let locator_hashes = chain.get_locator(hash);
            peer.send_get_headers(locator_hashes, None);
        }
    }

    pub fn handle_not_found(&self, peer: PeerRef, items: Vec<Inventory>) {
        let state = &mut self.state.write();
        let peer_state = &mut peer.state.write();

        for item in items {
            if !resolve_item(state, peer_state, &item) {
                let hash = match item {
                    Inventory::Transaction(txid) | Inventory::WitnessTransaction(txid) => {
                        txid.as_hash()
                    }
                    Inventory::Block(hash)
                    | Inventory::FilteredBlock(hash)
                    | Inventory::CompactBlock(hash)
                    | Inventory::WitnessBlock(hash)
                    | Inventory::WitnessFilteredBlock(hash) => hash.as_hash(),
                    Inventory::Error => continue,
                };
                warn!(
                    "Peer sent notfound for unrequested item: {} ({}).",
                    hash, peer.addr
                );

                // TODO: Ban

                return;
            }
        }
    }

    pub fn handle_get_cfheaders(
        &self,
        peer: PeerRef,
        filter_type: u8,
        start_height: u32,
        stop_hash: BlockHash,
    ) {
        if filter_type != 0 {
            return;
        }

        let filter_index = match &self.filter_index {
            None => return,
            Some(filter_index) => filter_index.read(),
        };

        let chain = self.chain.read();

        let hashes = chain.height_to_hash_range(start_height, &stop_hash, 2000);

        let filter_hashes = filter_index.filter_hashes_by_block_hashes(&hashes);

        let prev_filter_header = if start_height > 0 {
            let hash = chain.db.get_entry_by_height(start_height - 1).unwrap().hash;
            filter_index.filter_header_by_hash(hash).unwrap()
        } else {
            Default::default()
        };

        peer.send(NetworkMessage::CFHeaders(CFHeaders {
            filter_hashes,
            previous_filter: prev_filter_header,
            stop_hash,
            filter_type,
        }));
    }

    pub fn handle_get_cfilters(
        &self,
        peer: PeerRef,
        filter_type: u8,
        start_height: u32,
        stop_hash: BlockHash,
    ) {
        if filter_type != 0 {
            return;
        }

        let filter_index = match &self.filter_index {
            None => return,
            Some(filter_index) => filter_index.read(),
        };

        let hashes = self
            .chain
            .read()
            .height_to_hash_range(start_height, &stop_hash, 1000);
        let filters = filter_index.filters_by_block_hashes(&hashes);

        assert_eq!(hashes.len(), filters.len());

        for (hash, filter) in hashes.into_iter().zip(filters) {
            peer.send(NetworkMessage::CFilter(CFilter {
                block_hash: hash,
                filter: filter.content,
                filter_type,
            }));
        }
    }

    pub fn handle_get_block_txn(&self, peer: PeerRef, request: BlockTransactionsRequest) {
        let block = self.chain.read().db.get_block(request.block_hash).unwrap();
        if let Some(block) = block {
            if let Ok(transactions) = BlockTransactions::from_request(&request, &block) {
                peer.send(NetworkMessage::BlockTxn(BlockTxn { transactions }));
            }
        }
    }

    pub fn handle_message(&self, peer: PeerRef, message: NetworkMessage) {
        match message {
            NetworkMessage::Version(version) => {
                self.handle_version(peer, version);
            }

            NetworkMessage::Verack => {
                self.handle_verack(peer);
            }

            NetworkMessage::Addr(addrs) => {
                self.handle_addr(peer, addrs);
            }

            NetworkMessage::Inv(items) => {
                self.handle_inv(peer, items);
            }

            NetworkMessage::GetData(items) => {
                self.handle_get_data(peer, items);
            }

            NetworkMessage::NotFound(items) => {
                self.handle_not_found(peer, items);
            }

            NetworkMessage::GetBlocks(_) => {}

            NetworkMessage::GetHeaders(GetHeadersMessage {
                locator_hashes,
                stop_hash,
                ..
            }) => {
                self.handle_get_headers(peer, locator_hashes, stop_hash);
            }

            NetworkMessage::MemPool => {
                self.handle_mempool(peer);
            }

            NetworkMessage::Tx(tx) => {
                self.handle_tx(peer, tx);
            }

            NetworkMessage::Block(block) => {
                self.handle_block(peer, block);
            }

            NetworkMessage::Headers(headers) => {
                self.handle_headers(peer, headers);
            }

            NetworkMessage::GetAddr => {
                self.handle_get_addr(peer);
            }

            NetworkMessage::GetCFilters(GetCFilters {
                filter_type,
                start_height,
                stop_hash,
            }) => {
                self.handle_get_cfilters(peer, filter_type, start_height, stop_hash);
            }

            NetworkMessage::GetCFHeaders(GetCFHeaders {
                filter_type,
                start_height,
                stop_hash,
            }) => {
                self.handle_get_cfheaders(peer, filter_type, start_height, stop_hash);
            }

            NetworkMessage::GetCFCheckpt(_) => {}

            NetworkMessage::CmpctBlock(CmpctBlock { compact_block }) => {
                self.handle_compact_block(peer, compact_block);
            }

            NetworkMessage::GetBlockTxn(GetBlockTxn { txs_request }) => {
                self.handle_get_block_txn(peer, txs_request);
            }

            NetworkMessage::BlockTxn(BlockTxn {
                transactions:
                    BlockTransactions {
                        block_hash,
                        transactions,
                    },
            }) => {
                self.handle_block_txn(peer, transactions, block_hash);
            }

            NetworkMessage::SendHeaders => {}
            NetworkMessage::Ping(_) => {}
            NetworkMessage::Pong(_) => {}
            NetworkMessage::SendCmpct(_) => {}
            NetworkMessage::CFCheckpt(_) => {}
            NetworkMessage::CFHeaders(_) => {}
            NetworkMessage::CFilter(_) => {}
            NetworkMessage::MerkleBlock(_) => {}
            NetworkMessage::FeeFilter(_) => {}
            NetworkMessage::FilterLoad(_) => {}
            NetworkMessage::FilterAdd(_) => {}
            NetworkMessage::FilterClear => {}
            NetworkMessage::Alert(_) => {}
            NetworkMessage::Reject(_) => {}
        }
    }
}

fn resolve_item(state: &mut PoolState, peer_state: &mut State, item: &Inventory) -> bool {
    match item {
        Inventory::Error => false,
        Inventory::Transaction(txid) | Inventory::WitnessTransaction(txid) => {
            resolve_tx(state, peer_state, txid)
        }
        Inventory::Block(hash)
        | Inventory::FilteredBlock(hash)
        | Inventory::CompactBlock(hash)
        | Inventory::WitnessBlock(hash)
        | Inventory::WitnessFilteredBlock(hash) => resolve_block(state, peer_state, hash),
    }
}

fn resolve_block(state: &mut PoolState, peer_state: &mut State, hash: &BlockHash) -> bool {
    if !peer_state.block_map.remove(hash) {
        return false;
    }

    assert!(state.block_map.remove(hash));

    true
}

fn resolve_tx(state: &mut PoolState, peer_state: &mut State, txid: &Txid) -> bool {
    if !peer_state.tx_map.remove(txid) {
        return false;
    }

    assert!(state.tx_map.remove(txid));

    true
}
