use super::peer::{Peer, PeerListener};
use super::PeerId;
use crate::blockchain::Chain;
use crate::blockchain::ChainEntry;
use crate::blockchain::ChainListener;
use crate::protocol::NetworkParams;
use crate::util::{self, EmptyResult};
use bitcoin::secp256k1::rand::{seq::SliceRandom, thread_rng};
use bitcoin::{
    network::{message::NetworkMessage, message_blockdata::Inventory},
    Block, BlockHash, BlockHeader, Transaction,
};
use failure::Error;
use log::{debug, error, trace, warn};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{sync::Arc, time::Duration};
use tokio::stream::StreamExt;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::interval;

const BLOCK_DOWNLOAD_WINDOW: usize = 1024;
const MAX_BLOCKS_PER_PEER: usize = 128;

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

#[derive(Default)]
struct Peers {
    loader: Option<PeerId>,
    peers: HashMap<PeerId, Arc<Peer>>,
}

impl Peers {
    fn len(&self) -> usize {
        self.peers.len()
    }

    fn insert(&mut self, id: PeerId, peer: Arc<Peer>) {
        self.peers.insert(id, peer);
    }

    fn remove(&mut self, id: PeerId) -> Option<Arc<Peer>> {
        let peer = self.peers.remove(&id)?;
        if Some(peer.id) == self.loader {
            self.loader.take();
        }
        Some(peer)
    }
}

/// A collection of connected peers
pub struct P2P {
    addrs: Mutex<Addrs>,
    max_outbound: usize,
    peers: RwLock<Peers>,
    next_peer_id: AtomicUsize,
    chain: Arc<Mutex<Chain>>,
    block_map: RwLock<HashMap<BlockHash, PeerId>>,
    network_params: NetworkParams,
    resolve_headers_tx: mpsc::UnboundedSender<()>,
}

impl P2P {
    pub async fn new(
        chain: Arc<Mutex<Chain>>,
        network_params: NetworkParams,
        addrs: Vec<&str>,
        max_outbound: usize,
    ) -> Arc<Self> {
        let (resolve_headers_tx, resolve_headers_rx) = mpsc::unbounded_channel();
        let p2p = Arc::new(Self {
            max_outbound,
            addrs: Mutex::new(addrs.iter().filter_map(|addr| addr.parse().ok()).collect()),
            peers: RwLock::new(Default::default()),
            next_peer_id: Default::default(),
            chain: chain.clone(),
            block_map: RwLock::new(Default::default()),
            network_params,
            resolve_headers_tx,
        });
        p2p.clone().refill_peers();
        p2p.clone().resolve_headers_once(resolve_headers_rx);
        p2p
    }

    // use DNS seeds to find peers to connect to
    async fn discover_dns_seeds(&self) {
        use std::net::ToSocketAddrs;

        let mut addrs = tokio::task::block_in_place(move || {
            let mut seed_addrs = vec![];
            for seed in &self.network_params.dns_seeds {
                debug!("discovering addrs from {}", seed);
                if let Ok(addrs) = (*seed, self.network_params.p2p_port).to_socket_addrs() {
                    debug!("discovered {} addrs from {}", addrs.len(), seed);
                    seed_addrs.extend(addrs);
                } else {
                    warn!("failed to discover addrs from {}", seed);
                }
            }
            seed_addrs
        });

        debug!("discovered {} addrs from DNS seeds", addrs.len());
        addrs.shuffle(&mut thread_rng());
        let mut p2p_addrs = self.addrs.lock().await;
        for addr in addrs {
            p2p_addrs.insert(addr);
        }
    }

    // every 5 seconds check if we are connected to `max_outbound` peers
    // we don't support inbound connections yet
    fn refill_peers(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5));
            loop {
                interval.tick().await;

                let mut peers = self.peers.write().await;

                let need = self.max_outbound - peers.len();

                if need > 0 {
                    let mut addrs = self.addrs.lock().await;
                    debug!("refilling peers {}/{}", peers.len(), self.max_outbound);
                    for _ in 0..need {
                        if let Some(addr) = addrs.pop_front() {
                            debug!("connecting to {}", addr);
                            let id = self.next_peer_id.fetch_add(1, Ordering::SeqCst);

                            if peers.loader.is_none() {
                                peers.loader = Some(id);
                            }

                            let peer = Peer::from_outbound(
                                self.network_params.network,
                                id,
                                addr,
                                Some(self.clone()),
                            );
                            peers.insert(id, peer);
                        } else {
                            drop(addrs);
                            self.discover_dns_seeds().await;
                            break;
                        }
                    }
                }
            }
        });
    }
}

impl P2P {
    async fn handle_headers(&self, peer: &Peer, headers: Vec<BlockHeader>) -> EmptyResult {
        if headers.is_empty() {
            debug!("empty headers");
            return Ok(());
        }

        let mut last: Option<&BlockHeader> = None;
        for header in &headers {
            if last.is_some() && header.prev_blockhash != last.unwrap().block_hash() {
                warn!("Peer sent a bad header chain {}", peer.addr);
                peer.destroy.store(true, Ordering::SeqCst);
                return Ok(());
            }
            last = Some(header);
        }

        let mut last = None;

        {
            let mut chain = self.chain.lock().await;
            let mut peer_best = peer.best.lock().await;

            for header in &headers {
                let hash = header.block_hash();

                if chain.db.has_header(hash).unwrap() {
                    // We don't need these headers, but now we know this peer's tip hash
                    // Without a ChainEntry though, we don't know their height.
                    peer_best.hash = Some(hash);
                    trace!("already have header {} ({})", hash, peer.addr);
                    continue;
                }

                let entry = match chain.add_header(header) {
                    Ok((entry, _prev)) => entry,
                    Err(err) => {
                        return Err(err);
                    }
                };

                // Keep track of peer's best hash.
                peer_best.hash = Some(hash);

                // Keep track of peer's best height.
                peer_best.height = Some(entry.height);

                last = Some(entry);
            }
        }

        debug!("Received {} headers from peer {}", headers.len(), peer.addr);

        if headers.len() == 2000 {
            let locator = self
                .chain
                .lock()
                .await
                .get_locator(Some(last.expect("checked headers not empty").hash))?;
            peer.send_get_headers(locator, None).await;
        }

        self.resolve_headers();

        Ok(())
    }

    fn resolve_headers_once(self: Arc<Self>, mut resolve_headers_rx: mpsc::UnboundedReceiver<()>) {
        tokio::spawn(async move {
            loop {
                resolve_headers_rx.next().await;

                if !self.chain.lock().await.is_recent() {
                    continue;
                }

                if self.block_map.read().await.len() > BLOCK_DOWNLOAD_WINDOW - MAX_BLOCKS_PER_PEER {
                    continue;
                }

                let peers = self.peers.read().await;

                for (_, peer) in peers.peers.iter() {
                    self.resolve_headers_for_peer(peer, &*peers).await.unwrap();
                }
            }
        });
    }

    fn resolve_headers(&self) {
        let _ = self.resolve_headers_tx.send(());
    }

    async fn resolve_headers_for_peer(&self, peer: &Peer, peers: &Peers) -> EmptyResult {
        if !self.is_syncable(peer, peers).await {
            return Ok(());
        }

        if !peer.block_map.lock().await.is_empty() {
            return Ok(());
        }

        let best_hash = if let Some(best_hash) = peer.best.lock().await.hash {
            best_hash
        } else {
            return self.send_sync(peer, peers).await;
        };

        let best = {
            let mut chain = self.chain.lock().await;

            let best = if let Some(best) = chain.db.get_entry_by_hash(best_hash).unwrap() {
                *best
            } else {
                return Ok(());
            };

            if best.chainwork <= chain.tip.chainwork {
                return Ok(());
            }

            best
        };

        let staller = self.get_next_blocks(peer, best).await?;

        if let Some(staller) = staller {
            let slow = peers.peers.get(&staller);
            if let Some(slow) = slow {
                let stalling_since = slow.stalling_since.load(Ordering::SeqCst);
                let now = util::now() as usize;

                if stalling_since == 0 {
                    trace!("{} stalling", slow.addr);
                    slow.stalling_since.store(now, Ordering::SeqCst);
                } else if now > stalling_since + 2 {
                    slow.destroy.store(true, Ordering::SeqCst);
                    trace!(
                        "Peer is stalling block download window, set destroy=true {}",
                        slow.addr
                    );
                }
            }
        }

        Ok(())
    }

    async fn get_next_blocks(
        &self,
        peer: &Peer,
        best: ChainEntry,
    ) -> Result<Option<PeerId>, Error> {
        let mut items = vec![];
        let mut saved = vec![];
        let mut waiting = None;
        let mut staller = None;

        {
            let mut chain = self.chain.lock().await;
            let mut common = peer.common.lock().await;

            if common.hash.is_none() {
                let common_height = std::cmp::min(chain.height, best.height);
                let common_entry = chain
                    .db
                    .get_entry_by_height(common_height)?
                    .expect("height is at most chain height so entry must exist");
                common.height = Some(common_height);
                common.hash = Some(common_entry.hash);
            }

            let mut walker = *chain
                .db
                .get_entry_by_hash(common.hash.expect("common hash set above"))?
                .expect("common hash so must have that hash");

            walker = chain
                .common_ancestor(walker, best)?
                .expect("have block in common so must be an ancestor");

            common.hash = Some(walker.hash);
            common.height = Some(walker.height);

            let end_height =
                common.height.expect("set when common hash set") as usize + BLOCK_DOWNLOAD_WINDOW;
            let max_height = std::cmp::min(best.height, (end_height as u32) + 1);

            let block_map = self.block_map.read().await;

            while walker.height < max_height {
                let entries = chain
                    .db
                    .get_next_path(best, walker.height, MAX_BLOCKS_PER_PEER)?;

                if entries.is_empty() {
                    break;
                }

                let mut filled = false;

                for entry in &entries {
                    // This block is already queued for download.
                    if block_map.contains_key(&entry.hash) {
                        if waiting.is_none() {
                            waiting = block_map.get(&entry.hash).copied();
                        }
                        continue;
                    }

                    // The block is considered invalid.
                    if chain.db.has_invalid(&entry.hash) {
                        return Ok(staller);
                    }

                    // We have verified this block already (even if pruned).
                    if chain.db.is_main_chain(entry)? {
                        common.hash = Some(entry.hash);
                        common.height = Some(entry.height);
                        continue;
                    }

                    // We have this block already.
                    if chain.db.has_block(entry.hash).await? {
                        saved.push(*entry);
                        continue;
                    }

                    // We have reached the end of the window.
                    if entry.height > end_height as u32 {
                        // We could add to the request if the first in-flight
                        // block were to complete (or the window was one larger).
                        // Identify the peer with the first in-flight block as
                        // the slowest and stalling the download.
                        if items.is_empty() && waiting != Some(peer.id) {
                            staller = waiting;
                            break;
                        }
                    } else {
                        items.push(entry.hash);
                    }

                    // Stay within peer limit.
                    if items.len() >= MAX_BLOCKS_PER_PEER {
                        filled = true;
                        break;
                    }
                }

                if filled {
                    break;
                }

                walker = *entries.back().expect("non empty checked above");
            }
        }

        if !saved.is_empty() {
            self.attach_block(saved[0]).await;
        }

        if items.is_empty() {
            return Ok(staller);
        }

        self.get_block(peer, items).await;

        Ok(staller)
    }

    async fn attach_block(&self, entry: ChainEntry) {
        if let Err(err) = self.chain.lock().await.attach(entry).await {
            warn!("{}", err);
        }
    }

    async fn get_block(&self, peer: &Peer, hashes: Vec<BlockHash>) {
        let mut items = vec![];
        let now = util::now();

        {
            let mut peer_block_map = peer.block_map.lock().await;
            let chain = self.chain.lock().await;
            let mut block_map = self.block_map.write().await;

            for hash in hashes {
                if block_map.contains_key(&hash) || chain.db.has_block(hash).await.unwrap() {
                    continue;
                }

                block_map.insert(hash, peer.id);
                peer_block_map.insert(hash, now as u32);

                items.push(hash);
            }

            if items.is_empty() {
                return;
            }

            debug!(
                "Requesting {}/{} blocks from peer with getdata ({}).",
                items.len(),
                block_map.len(),
                peer.addr
            );
        }

        peer.send(NetworkMessage::GetData(
            items.into_iter().map(Inventory::WitnessBlock).collect(),
        ))
        .await;

        peer.block_time
            .store(util::now() as usize, Ordering::SeqCst);
    }

    async fn handle_block(&self, peer: &Peer, block: Block) -> EmptyResult {
        let hash = block.block_hash();

        peer.block_map.lock().await.remove(&hash);

        let was_requested = self.block_map.read().await.contains_key(&hash);

        if !was_requested {
            peer.destroy.store(true, Ordering::SeqCst);
            return Ok(());
        }

        peer.block_time
            .store(util::now() as usize, Ordering::SeqCst);

        peer.stalling_since.store(0, Ordering::SeqCst);

        {
            // lock chain before block map so that a block is always seen as in the chain or block map
            let mut chain = self.chain.lock().await;

            self.block_map.write().await.remove(&hash);

            chain.add(block).await?;
        }

        self.resolve_headers();

        Ok(())
    }

    async fn handle_get_headers(
        &self,
        peer: &Peer,
        locator: Vec<BlockHash>,
        stop: BlockHash,
    ) -> EmptyResult {
        let stop = if stop == BlockHash::default() {
            None
        } else {
            Some(stop)
        };

        let mut headers = vec![];

        {
            let mut chain = self.chain.lock().await;

            let hash = if locator.is_empty() {
                Some(BlockHash::default())
            } else {
                let common = chain.find_locator(locator)?;
                chain.db.get_next_hash(common)?
            };

            let mut entry = if let Some(hash) = hash {
                chain.db.get_entry_by_hash(hash)?.copied()
            } else {
                None
            };

            while let Some(e) = entry {
                headers.push(e.to_header());

                if let Some(stop) = stop {
                    if e.hash == stop {
                        break;
                    }
                }

                if headers.len() == 2000 {
                    break;
                }

                entry = chain.db.get_next(&e)?.copied();
            }
        }

        peer.send(NetworkMessage::Headers(headers)).await;

        Ok(())
    }

    async fn is_syncable(&self, peer: &Peer, peers: &Peers) -> bool {
        if peer.destroyed.load(Ordering::SeqCst) {
            return false;
        }

        if peer.destroy.load(Ordering::SeqCst) {
            return false;
        }

        if !peer.handshake.load(Ordering::SeqCst) {
            return false;
        }

        if peers.loader != Some(peer.id) && !self.chain.lock().await.is_recent() {
            return false;
        }

        true
    }

    async fn send_sync(&self, peer: &Peer, peers: &Peers) -> EmptyResult {
        if peer.syncing.load(Ordering::SeqCst) {
            return Ok(());
        }

        if !self.is_syncable(peer, peers).await {
            return Ok(());
        }

        peer.syncing.store(true, Ordering::SeqCst);

        let locator = {
            let mut chain = self.chain.lock().await;
            let best = chain.most_work();
            let prev = chain.db.get_entry_by_hash(best.prev_block)?.copied();
            chain.get_locator(Some(prev.unwrap_or(best).hash))?
        };

        peer.send_get_headers(locator, None).await;

        Ok(())
    }
}

#[async_trait::async_trait]
impl PeerListener for P2P {
    async fn handle_packet(&self, peer: &Peer, packet: NetworkMessage) {
        let res = match packet {
            NetworkMessage::Headers(headers) => self.handle_headers(peer, headers).await,
            NetworkMessage::Block(block) => self.handle_block(peer, block).await,
            NetworkMessage::GetHeaders(msg) => {
                self.handle_get_headers(peer, msg.locator_hashes, msg.stop_hash)
                    .await
            }
            _ => Ok(()),
        };
        if let Err(err) = res {
            error!("error handling packet from {} {}", peer.addr, err);
            peer.destroy.store(true, Ordering::SeqCst);
        }
    }
    async fn handle_close(&self, peer: &Peer, _connected: bool) {
        debug!("disconnected from {}", peer.addr);

        let mut peers = self.peers.write().await;
        peers.remove(peer.id);
        if peers.loader == Some(peer.id) {
            peers.loader = None;
        }
        let mut block_map = self.block_map.write().await;

        let peer_block_map = peer.block_map.lock().await.drain().collect::<Vec<_>>();

        for (hash, _) in peer_block_map {
            block_map.remove(&hash);
        }

        self.resolve_headers();
    }

    async fn handle_open(&self, peer: &Peer) {
        debug!("handshake complete with {}", peer.addr);
        peer.send(NetworkMessage::SendHeaders).await;
        let peers = self.peers.read().await;
        if self.send_sync(peer, &*peers).await.is_err() {
            peer.destroy.store(true, Ordering::SeqCst);
        }
    }

    async fn handle_error(&self, peer: &Peer, error: &Error) {
        debug!("error: {} from peer {}", error, peer.addr);
    }

    async fn handle_ban(&self, _peer: &Peer) {}

    async fn handle_connect(&self, peer: &Peer) {
        debug!("connected to {}", peer.addr);
    }
}

impl ChainListener for P2P {}
