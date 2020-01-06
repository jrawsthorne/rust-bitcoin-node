use super::peer::{Peer, PeerListener};
use crate::blockchain::ChainEntry;
use crate::blockchain::ChainListener;
use crate::mempool::MempoolListener;
use crate::protocol::NetworkParams;
use bitcoin::{network::message::NetworkMessage, Block, Transaction};
use failure::Error;
use log::debug;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio::time::interval;

/// Trait that handles P2P events
pub trait P2PListener {
    fn handle_tx(&self, _tx: &Transaction) {}
    fn handle_error(&self, _error: &Error) {}
    fn handle_connection(&self, _addr: &SocketAddr) {}
    fn handle_listening(&self) {}
    fn handle_block(&self, _block: &Block, _entry: &ChainEntry) {}
    fn handle_full(&self) {}
    fn handle_loader(&self, _peer: &Peer) {}
    fn handle_packet(&self, _packet: &NetworkMessage, _peer: &Peer) {}
    fn handle_peer_connect(&self, _peer: &Peer) {}
    fn handle_peer_open(&self, _peer: &Peer) {}
    fn handle_peer_close(&self, _peer: &Peer, _connected: bool) {}
    fn handle_ban(&self, _peer: &Peer) {}
    fn handle_peer(&self, _peer: &Peer) {}
    fn handle_timeout(&self) {}
    fn handle_ack(&self, _peer: &Peer) {}
    fn handle_reject(&self, _peer: &Peer) {}
    fn handle_open(&self) {}
}

#[derive(Default)]
struct Addrs {
    set: HashSet<SocketAddr>,
    list: VecDeque<SocketAddr>,
}

impl Addrs {
    pub fn contains(&self, addr: &SocketAddr) -> bool {
        self.set.contains(addr)
    }

    pub fn pop_front(&mut self) -> Option<SocketAddr> {
        let addr = self.list.pop_front();
        if let Some(addr) = &addr {
            self.set.remove(addr);
        }
        addr
    }

    pub fn insert(&mut self, addr: SocketAddr) {
        self.list.push_back(addr);
        self.set.insert(addr);
    }
}

impl std::iter::FromIterator<SocketAddr> for Addrs {
    fn from_iter<T: IntoIterator<Item = SocketAddr>>(iter: T) -> Self {
        let mut addrs = Addrs::default();
        for addr in iter {
            addrs.insert(addr);
        }
        addrs
    }
}

/// A collection of connected peers
pub struct P2P {
    addrs: Mutex<Addrs>,
    max_outbound: usize,
    peers: Mutex<HashMap<SocketAddr, Arc<Peer>>>,
}

impl P2P {
    pub fn new(network_params: NetworkParams, addrs: Vec<&str>, max_outbound: usize) -> Arc<Self> {
        let p2p = Arc::new(Self {
            max_outbound,
            addrs: Mutex::new(addrs.iter().filter_map(|addr| addr.parse().ok()).collect()),
            peers: Default::default(),
        });
        Self::discover_dns_seeds(p2p.clone(), network_params.dns_seeds);
        Self::refill_peers(p2p.clone());
        p2p
    }

    // use DNS seeds to find peers to connect to
    fn discover_dns_seeds(p2p: Arc<P2P>, seeds: Vec<&'static str>) {
        tokio::spawn(async move {
            use std::net::ToSocketAddrs;
            use tokio::sync::oneshot;
            let (tx, rx) = oneshot::channel();
            tokio::task::spawn_blocking(move || {
                let mut addrs: Vec<SocketAddr> = vec![];
                for seed in seeds.iter() {
                    debug!("discovering addrs from {}", seed);
                    if let Ok(a) = format!("{}:8333", seed).to_socket_addrs() {
                        addrs.extend(a);
                    }
                }
                tx.send(addrs).unwrap();
            });
            let addrs: Vec<SocketAddr> = rx.await.unwrap();
            debug!("discovered {} addrs from DNS seeds", addrs.len());
            let mut p2p_addrs = p2p.addrs.lock().await;
            for addr in addrs {
                p2p_addrs.insert(addr);
            }
        });
    }

    // every 5 seconds check if we are connected to `max_outbound` peers
    // we don't support inbound connections yet
    fn refill_peers(p2p: Arc<P2P>) {
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5));
            loop {
                interval.tick().await;

                let mut peers = p2p.peers.lock().await;

                let need = p2p.max_outbound - peers.len();

                if need > 0 {
                    let mut addrs = p2p.addrs.lock().await;
                    debug!("refilling peers {}/{}", peers.len(), p2p.max_outbound);
                    for _ in 0..need {
                        if let Some(addr) = addrs.pop_front() {
                            debug!("connecting to {}", addr);
                            let peer = Peer::from_outbound(addr, Some(p2p.clone()));
                            peers.insert(addr, peer);
                        } else {
                            break;
                        }
                    }
                }
            }
        });
    }
}

impl MempoolListener for P2P {}

#[async_trait::async_trait]
impl PeerListener for P2P {
    async fn handle_packet(&self, _peer: &Peer, _packet: NetworkMessage) {}
    async fn handle_close(&self, peer: &Peer, _connected: bool) {
        debug!("disconnected from {}", peer.addr);
        self.peers.lock().await.remove(&peer.addr);
    }
    async fn handle_open(&self, peer: &Peer) {
        debug!("handshake complete with {}", peer.addr);
    }
    async fn handle_error(&self, peer: &Peer, error: &Error) {
        debug!("error: {} from peer {}", error, peer.addr);
    }
    async fn handle_ban(&self, peer: &Peer) {
        self.peers.lock().await.remove(&peer.addr);
    }
    async fn handle_connect(&self, peer: &Peer) {
        debug!("connected to {}", peer.addr);
    }
}

impl ChainListener for P2P {}
