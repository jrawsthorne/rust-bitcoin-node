use super::peer::Peer;
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
use tokio::sync::Notify;
use tokio::sync::{Mutex, RwLock};
use tokio::time::interval;

const BLOCK_DOWNLOAD_WINDOW: usize = 1024;
const MAX_BLOCKS_PER_PEER: usize = 16;

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

pub type P2PManager = P2P;

/// A collection of connected peers
pub struct P2P {
    addrs: Arc<Mutex<Addrs>>,
    max_outbound: usize,
    peers: Arc<RwLock<Peers>>,
    next_peer_id: Arc<AtomicUsize>,
    chain: Arc<Mutex<Chain>>,
    block_map: Arc<RwLock<HashMap<BlockHash, PeerId>>>,
    network_params: NetworkParams,
    verifying: Arc<RwLock<HashSet<BlockHash>>>,
    resolve_headers_notify: Arc<Notify>,
}

impl Clone for P2P {
    fn clone(&self) -> Self {
        Self {
            addrs: Arc::clone(&self.addrs),
            max_outbound: self.max_outbound,
            peers: Arc::clone(&self.peers),
            next_peer_id: Arc::clone(&self.next_peer_id),
            chain: Arc::clone(&self.chain),
            block_map: Arc::clone(&self.block_map),
            network_params: self.network_params.clone(),
            verifying: Arc::clone(&self.verifying),
            resolve_headers_notify: Arc::clone(&self.resolve_headers_notify),
        }
    }
}

impl P2P {
    pub fn new(
        chain: Arc<Mutex<Chain>>,
        network_params: NetworkParams,
        addrs: Vec<&str>,
        max_outbound: usize,
    ) -> Self {
        let p2p = Self {
            addrs: Arc::new(Mutex::new(
                addrs.iter().filter_map(|addr| addr.parse().ok()).collect(),
            )),
            max_outbound,
            peers: Default::default(),
            next_peer_id: Default::default(),
            chain: chain,
            block_map: Default::default(),
            network_params,
            verifying: Default::default(),
            resolve_headers_notify: Default::default(),
        };
        p2p.clone().maintain_peers();
        p2p.clone().resolve_headers_task();
        p2p
    }

    async fn discover_dns_seeds(&self) {
        use std::net::ToSocketAddrs;
        let mut seed_addrs: Vec<SocketAddr> = tokio::task::block_in_place(|| {
            self.network_params
                .dns_seeds
                .iter()
                .filter_map(|seed| {
                    debug!("discovering addrs from {}", seed);
                    let addrs = (*seed, self.network_params.p2p_port).to_socket_addrs().ok();
                    if let Some(addrs) = &addrs {
                        debug!("discovered {} addrs from {}", addrs.len(), seed);
                    } else {
                        warn!("failed to discover addrs from {}", seed);
                    }
                    addrs
                })
                .flatten()
                .collect()
        });
        debug!("discovered {} addrs from DNS seeds", seed_addrs.len());
        seed_addrs.shuffle(&mut thread_rng());
        let mut addrs = self.addrs.lock().await;
        for addr in seed_addrs {
            addrs.insert(addr);
        }
    }

    fn maintain_peers(self) {
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                self.refill_peers().await;
            }
        });
    }

    async fn refill_peers(&self) {
        let mut peers = self.peers.write().await;
        let mut addrs = self.addrs.lock().await;

        let need = self.max_outbound - peers.len();

        if need > 0 {
            debug!("refilling peers {}/{}", peers.len(), self.max_outbound);
            for _ in 0..need {
                if let Some(addr) = addrs.pop_front() {
                    debug!("connecting to {}", addr);
                    let id = self.next_peer_id.fetch_add(1, Ordering::SeqCst);

                    if peers.loader.is_none() {
                        peers.loader = Some(id);
                    }

                    let peer =
                        Peer::from_outbound(self.network_params.network, id, addr, self.clone());
                    peers.insert(id, peer);
                } else {
                    drop(addrs);
                    drop(peers);
                    self.discover_dns_seeds().await;
                    return;
                }
            }
        }
    }

    async fn handle_headers(&self, peer: &Peer, headers: Vec<BlockHeader>) -> EmptyResult {
        if headers.is_empty() {
            return Ok(());
        }

        let mut last: Option<&BlockHeader> = None;
        for header in &headers {
            if last.is_some() && header.prev_blockhash != last.unwrap().block_hash() {
                warn!("destroy: Peer sent a bad header chain {}", peer.addr);
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

        self.queue_resolve_headers();

        Ok(())
    }
}

impl P2P {
    fn resolve_headers_task(self) {
        tokio::spawn(async move {
            loop {
                self.resolve_headers_notify.notified().await;
                self.resolve_headers().await;
            }
        });
    }

    fn queue_resolve_headers(&self) {
        self.resolve_headers_notify.notify();
    }

    async fn resolve_headers(&self) {
        {
            let chain = self.chain.lock().await;
            if !chain.is_recent() {
                return;
            }
        }

        if self.block_map.read().await.len() > BLOCK_DOWNLOAD_WINDOW - MAX_BLOCKS_PER_PEER {
            return;
        }

        let peers = self.clone_peers().await;
        for peer in peers {
            self.resolve_headers_for_peer(&peer).await.unwrap();
        }
    }

    async fn clone_peers(&self) -> Vec<Arc<Peer>> {
        self.peers
            .read()
            .await
            .peers
            .values()
            .map(|p| Arc::clone(p))
            .collect::<Vec<_>>()
    }

    async fn resolve_headers_for_peer(&self, peer: &Peer) -> EmptyResult {
        if !self.is_syncable(peer).await {
            return Ok(());
        }

        if !peer.block_map.lock().await.is_empty() {
            return Ok(());
        }

        let best_hash = if let Some(best_hash) = peer.best.lock().await.hash {
            best_hash
        } else {
            return self.send_sync(peer).await;
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
            let peers = self.peers.read().await;
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
            let verifying = self.verifying.read().await;

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

                    if verifying.contains(&entry.hash) {
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
        self.chain
            .lock()
            .await
            .attach(entry)
            .await
            .expect("bad block");
    }

    async fn get_block(&self, peer: &Peer, hashes: Vec<BlockHash>) {
        let mut items = vec![];
        let now = util::now();

        {
            let chain = self.chain.lock().await;
            let mut peer_block_map = peer.block_map.lock().await;
            let mut block_map = self.block_map.write().await;
            let verifying = self.verifying.read().await;

            for hash in hashes {
                if block_map.contains_key(&hash)
                    || chain.db.has_block(hash).await.unwrap()
                    || verifying.contains(&hash)
                {
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
            trace!("destroy: block {} not requested {} ", hash, peer.addr);

            peer.destroy.store(true, Ordering::SeqCst);
            return Ok(());
        }

        peer.block_time
            .store(util::now() as usize, Ordering::SeqCst);

        peer.stalling_since.store(0, Ordering::SeqCst);

        let mut chain = self.chain.lock().await;

        {
            self.block_map.write().await.remove(&hash);
            self.verifying.write().await.insert(hash);
        }

        chain.add(block).await.unwrap();
        self.queue_resolve_headers();

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

    async fn is_syncable(&self, peer: &Peer) -> bool {
        if peer.destroyed.load(Ordering::SeqCst) {
            return false;
        }

        if peer.destroy.load(Ordering::SeqCst) {
            return false;
        }

        if !peer.handshake.load(Ordering::SeqCst) {
            return false;
        }

        {
            let chain = self.chain.lock().await;
            let peers = self.peers.read().await;
            if peers.loader != Some(peer.id) && !chain.is_recent() {
                return false;
            }
        }

        true
    }

    async fn send_sync(&self, peer: &Peer) -> EmptyResult {
        if peer.syncing.load(Ordering::SeqCst) {
            return Ok(());
        }

        if !self.is_syncable(peer).await {
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

impl P2P {
    pub async fn handle_packet(&self, peer: &Peer, packet: NetworkMessage) {
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
            error!("destroy: error handling packet from {} {}", peer.addr, err);
            peer.destroy.store(true, Ordering::SeqCst);
        }
    }

    pub async fn handle_close(&self, peer: &Peer, _connected: bool) {
        debug!("disconnected from {}", peer.addr);

        {
            let mut peers = self.peers.write().await;
            peers.remove(peer.id);
            if peers.loader == Some(peer.id) {
                peers.loader = None;
            }
        }

        {
            let peer_block_map = peer.block_map.lock().await.drain().collect::<Vec<_>>();
            let mut block_map = self.block_map.write().await;

            for (hash, _) in peer_block_map {
                block_map.remove(&hash);
            }
        }

        self.queue_resolve_headers();
    }

    pub async fn handle_open(&self, peer: &Peer) {
        debug!("handshake complete with {}", peer.addr);
        peer.send(NetworkMessage::SendHeaders).await;
        if self.send_sync(peer).await.is_err() {
            trace!("destroy: send sync error {} ", peer.addr);

            peer.destroy.store(true, Ordering::SeqCst);
        }
        self.queue_resolve_headers();
    }

    pub async fn handle_error(&self, peer: &Peer, error: &Error) {
        debug!("error: {} from peer {}", error, peer.addr);
    }

    pub async fn handle_ban(&self, _peer: &Peer) {}

    pub async fn handle_connect(&self, peer: &Peer) {
        debug!("connected to {}", peer.addr);
    }
}

impl ChainListener for P2P {}
