use crate::{blockchain::Chain, mempool::MemPool};
use bitcoin::{
    network::{
        constants::ServiceFlags,
        message::{NetworkMessage, RawNetworkMessage},
        message_blockdata::{GetHeadersMessage, Inventory},
        message_network::VersionMessage,
        stream_reader::StreamReader,
    },
    Block, BlockHash, BlockHeader, Network, Transaction,
};
use log::{debug, trace, warn};
use maplit::hashmap;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{HashMap, HashSet},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    thread::JoinHandle,
};

pub type SharedChain = Arc<RwLock<Chain>>;
pub type SharedMempool = Arc<RwLock<MemPool>>;

pub struct P2P {
    peers: HashMap<usize, Peer>,
    chain: SharedChain,
    network: Network,
    requested_blocks: HashSet<BlockHash>,
    requested_blocks_by_peer: HashMap<usize, HashSet<BlockHash>>,
    mempool: SharedMempool,
}

impl P2P {
    pub fn new(
        chain: SharedChain,
        network: Network,
        mempool: SharedMempool,
        addr: &str,
    ) -> SharedP2P {
        let peer = Peer::new(addr.parse().unwrap(), 0, network);

        let p2p = Arc::new(Mutex::new(P2P {
            peers: hashmap! {0 => peer},
            network,
            chain: chain.clone(),
            requested_blocks: HashSet::default(),
            requested_blocks_by_peer: hashmap! {0 => HashSet::default()},
            mempool,
        }));

        let jh = p2p.lock().peers.get_mut(&0).unwrap().run(p2p.clone());

        jh.join().unwrap();

        p2p
    }

    pub fn handle_headers(&mut self, headers: Vec<BlockHeader>, id: usize) {
        {
            let mut chain = self.chain.write();

            if headers.is_empty() {
                return;
            }
            for header in &headers {
                if !chain.db.has_header(&header.block_hash()) {
                    chain.add_header(header).unwrap();
                }
            }
        }
        // ask for more
        if headers.len() == 2000 {
            let locator = self
                .chain
                .read()
                .get_locator(Some(headers[headers.len() - 1].block_hash()));
            self.send(
                id,
                NetworkMessage::GetHeaders(GetHeadersMessage::new(locator, BlockHash::default())),
            )
        }

        if self.chain.read().is_recent() {
            self.request_next_blocks(id);
        }
    }

    pub fn handle_block(&mut self, block: Block, peer: usize) {
        let hash = block.block_hash();
        assert!(self.requested_blocks.remove(&hash));
        self.requested_blocks_by_peer
            .get_mut(&peer)
            .unwrap()
            .remove(&hash);
        self.chain.write().add(block).unwrap();
    }

    fn send(&mut self, peer: usize, message: NetworkMessage) {
        self.peers.get_mut(&peer).map(|peer| peer.send(message));
    }

    fn request_next_blocks(&mut self, peer: usize) {
        let mut blocks_to_download = vec![];

        {
            let requested_blocks = &mut self.requested_blocks;
            let peer_requested_blocks = &mut self.requested_blocks_by_peer.get_mut(&peer).unwrap();
            let chain = self.chain.read();

            let mut height = chain.tip.height;

            while blocks_to_download.len() < 16 {
                let next = chain.db.get_next_path(chain.most_work(), height, 16);
                if next.is_empty() {
                    break;
                }
                for header in next {
                    if !chain.db.has_block(header.hash).unwrap()
                        && requested_blocks.insert(header.hash)
                    {
                        peer_requested_blocks.insert(header.hash);
                        blocks_to_download.push(Inventory::WitnessBlock(header.hash));
                    }
                }
                height += 16;
            }
        }

        self.send(peer, NetworkMessage::GetData(blocks_to_download));
    }

    fn handle_tx(&mut self, tx: Transaction) {
        let chain = self.chain.read();
        let txid = tx.txid();
        if let Err(err) = self.mempool.write().add_tx(&chain, tx) {
            warn!("tx {} couldn't be added to mempool {}", txid, err);
        }
    }

    pub fn handle_message(&mut self, message: RawNetworkMessage, id: usize) {
        match message.payload {
            NetworkMessage::Headers(headers) => self.handle_headers(headers, id),
            NetworkMessage::Version(version) => {
                dbg!(version);
                self.send(id, NetworkMessage::Verack);
            }
            NetworkMessage::Ping(nonce) => self.send(id, NetworkMessage::Pong(nonce)),
            NetworkMessage::Verack => {
                self.send(id, NetworkMessage::SendHeaders);

                let locator = {
                    let chain = self.chain.read();

                    let most_work = chain.most_work();

                    // start at one before most work header or genesis block
                    let start = chain
                        .db
                        .get_entry_by_hash(&most_work.prev_block)
                        .map(|e| e.hash)
                        .unwrap_or(most_work.hash);

                    chain.get_locator(Some(start))
                };

                self.send(
                    id,
                    NetworkMessage::GetHeaders(GetHeadersMessage::new(
                        locator,
                        BlockHash::default(),
                    )),
                );
            }
            NetworkMessage::Block(block) => {
                self.handle_block(block, id);
                if self.requested_blocks_by_peer[&id].is_empty() {
                    self.request_next_blocks(id);
                }
            }
            NetworkMessage::Tx(tx) => self.handle_tx(tx),
            NetworkMessage::Inv(invs) => {
                self.send(
                    id,
                    NetworkMessage::GetData(
                        invs.into_iter()
                            .filter(|inv| match inv {
                                Inventory::Transaction(_) => true,
                                _ => false,
                            })
                            .collect(),
                    ),
                );
            }
            _ => {}
        }
    }
}

pub type SharedP2P = Arc<Mutex<P2P>>;

pub struct Peer {
    writer: TcpStream,
    addr: SocketAddr,
    id: usize,
    network: Network,
}

impl Peer {
    pub fn run(&mut self, p2p: SharedP2P) -> JoinHandle<()> {
        let reader = self.writer.try_clone().unwrap();
        let id = self.id;
        let addr = self.addr;
        self.send_version();

        std::thread::spawn(move || {
            let mut reader = StreamReader::new(reader, None);
            while let Ok(message) = reader.read_next::<RawNetworkMessage>() {
                trace!("received {} from {}", message.cmd(), addr);
                p2p.lock().handle_message(message, id);
            }

            debug!("{} closed", addr);
        })
    }

    pub fn new(addr: SocketAddr, id: usize, network: Network) -> Self {
        let writer = TcpStream::connect(addr).unwrap();
        Self {
            writer,
            addr,
            id,
            network,
        }
    }

    pub fn send(&mut self, message: NetworkMessage) {
        use bitcoin::consensus::serialize;
        use std::io::Write;

        let cmd = message.cmd();

        let bytes = serialize(&RawNetworkMessage {
            payload: message,
            magic: self.network.magic(),
        });

        self.writer.write_all(&bytes).unwrap();

        trace!("sent {} to {}", cmd, self.addr);
    }

    fn send_version(&mut self) {
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
            relay: true,
        };
        self.send(NetworkMessage::Version(version));
    }
}
