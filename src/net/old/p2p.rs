use super::peer::Peer;
use super::PeerId;
use crate::blockchain::Chain;
use crate::blockchain::ChainEntry;
use crate::protocol::NetworkParams;
use crate::util::{self, EmptyResult};
use bitcoin::secp256k1::rand::{seq::SliceRandom, thread_rng};
use bitcoin::{
    network::{message::NetworkMessage, message_blockdata::Inventory},
    Block, BlockHash, BlockHeader,
};
use failure::{err_msg, Error};
use log::{debug, error, trace, warn};
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::Notify;
use tokio::time::interval;

const BLOCK_DOWNLOAD_WINDOW: usize = 1024;
const MAX_BLOCKS_PER_PEER: usize = 16;

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

pub type P2PManager = Arc<P2P>;

/// A collection of connected peers
pub struct P2P {
    addrs: Mutex<Addrs>,
    max_outbound: usize,
    peers: RwLock<Peers>,
    next_peer_id: AtomicUsize,
    chain: Arc<RwLock<Chain>>,
    block_map: RwLock<HashMap<BlockHash, PeerId>>,
    network_params: NetworkParams,
    verifying: RwLock<HashSet<BlockHash>>,
    resolve_headers_notify: Notify,
}

impl P2P {
    pub fn new(
        chain: Arc<RwLock<Chain>>,
        network_params: NetworkParams,
        addrs: Vec<&str>,
        max_outbound: usize,
    ) -> Arc<Self> {
        let p2p = Arc::new(Self {
            addrs: Mutex::new(addrs.iter().filter_map(|addr| addr.parse().ok()).collect()),
            max_outbound,
            peers: Default::default(),
            next_peer_id: Default::default(),
            chain: chain,
            block_map: Default::default(),
            network_params,
            verifying: Default::default(),
            resolve_headers_notify: Default::default(),
        });
        tokio::spawn(p2p.clone().maintain_peers());
        tokio::spawn(p2p.clone().resolve_headers_task());
        p2p
    }

    async fn discover_dns_seeds(&self) {
        use tokio::net::lookup_host;

        let mut seed_addrs = vec![];

        for seed in &self.network_params.dns_seeds {
            debug!("discovering addrs from {}", seed);
            match lookup_host((*seed, self.network_params.p2p_port)).await {
                Ok(addrs) => {
                    let addrs: Vec<SocketAddr> = addrs.collect();
                    debug!("discovered {} addrs from {}", addrs.len(), seed);
                    seed_addrs.extend(addrs);
                }
                Err(_) => warn!("failed to discover addrs from {}", seed),
            }
        }

        debug!("discovered {} addrs from DNS seeds", seed_addrs.len());
        seed_addrs.shuffle(&mut thread_rng());
        let mut addrs = self.addrs.lock();
        for addr in seed_addrs {
            addrs.insert(addr);
        }
    }

    async fn maintain_peers(self: Arc<Self>) {
        let mut interval = interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            self.refill_peers().await;
        }
    }

    async fn refill_peers(self: &Arc<Self>) {
        let mut dns = false;

        {
            let mut peers = self.peers.write();
            let mut addrs = self.addrs.lock();

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

                        let peer = Peer::from_outbound(
                            self.network_params.network,
                            id,
                            addr,
                            self.clone(),
                        );
                        peers.insert(id, peer);
                    } else {
                        dns = true;
                        break;
                    }
                }
            }
        }

        if dns {
            self.discover_dns_seeds().await;
        }
    }

    fn handle_headers(&self, peer: &Peer, headers: Vec<BlockHeader>) -> EmptyResult {
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

        {
            let mut chain = self.chain.write();
            let mut peer_best = peer.best.lock();

            for header in &headers {
                let hash = header.block_hash();

                if chain.db.has_header(&hash) {
                    // We don't need these headers, but now we know this peer's tip hash
                    // Without a ChainEntry though, we don't know their height.
                    peer_best.hash = Some(hash);
                    trace!("already have header {} ({})", hash, peer.addr);
                    continue;
                }

                let entry = match chain.add_header(&header) {
                    Ok((entry, _prev)) => entry,
                    Err(_err) => {
                        return Err(err_msg("bad header"));
                    }
                };

                // Keep track of peer's best hash.
                peer_best.hash = Some(hash);

                // Keep track of peer's best height.
                peer_best.height = Some(entry.height);
            }
        }

        debug!("Received {} headers from peer {}", headers.len(), peer.addr);

        if headers.len() == 2000 {
            let locator = self
                .chain
                .read()
                .get_locator(Some(headers[headers.len() - 1].block_hash()));
            peer.send_get_headers(locator, None);
        }

        self.queue_resolve_headers();

        Ok(())
    }
}

impl P2P {
    async fn resolve_headers_task(self: Arc<Self>) {
        loop {
            self.resolve_headers_notify.notified().await;
            debug!("resolve headers");
            self.resolve_headers();
            debug!("resolve headers done");
        }
    }

    fn queue_resolve_headers(&self) {
        self.resolve_headers_notify.notify();
    }

    fn resolve_headers(&self) {
        if !self.chain.read().is_recent() {
            return;
        }

        if self.block_map.read().len() > BLOCK_DOWNLOAD_WINDOW - MAX_BLOCKS_PER_PEER {
            return;
        }

        for peer in self.peers.read().peers.values() {
            self.resolve_headers_for_peer(peer).unwrap();
        }
    }

    fn resolve_headers_for_peer(&self, peer: &Peer) -> EmptyResult {
        if !self.is_syncable(peer) {
            return Ok(());
        }

        if !peer.block_map.lock().is_empty() {
            return Ok(());
        }

        let best_hash = {
            let b: Option<BlockHash> = peer.best.lock().hash;
            match b {
                None => return self.send_sync(peer),
                Some(best_hash) => best_hash,
            }
        };

        let best = {
            let chain = self.chain.read();

            let best = if let Some(best) = chain.db.get_entry_by_hash(&best_hash) {
                *best
            } else {
                return Ok(());
            };

            if best.chainwork <= chain.tip.chainwork {
                return Ok(());
            }

            best
        };

        let staller = self.get_next_blocks(peer, best)?;

        if let Some(staller) = staller {
            let peers = self.peers.read();
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

    fn get_next_blocks(&self, peer: &Peer, best: ChainEntry) -> Result<Option<PeerId>, Error> {
        let mut items = vec![];
        let mut saved = vec![];
        let mut waiting = None;
        let mut staller = None;

        {
            let chain = self.chain.read();
            let mut common = peer.common.lock();

            if common.hash.is_none() {
                let common_height = std::cmp::min(chain.height, best.height);
                let common_entry = chain
                    .db
                    .get_entry_by_height(&common_height)
                    .expect("height is at most chain height so entry must exist");
                common.height = Some(common_height);
                common.hash = Some(common_entry.hash);
            }

            let mut walker = chain
                .db
                .get_entry_by_hash(&common.hash.expect("common hash set above"))
                .expect("common hash so must have that hash");

            walker = chain
                .common_ancestor(&walker, &best)
                .expect("have block in common so must be an ancestor");

            common.hash = Some(walker.hash);
            common.height = Some(walker.height);

            let end_height =
                common.height.expect("set when common hash set") as usize + BLOCK_DOWNLOAD_WINDOW;
            let max_height = std::cmp::min(best.height, (end_height as u32) + 1);

            let block_map = self.block_map.read();
            let verifying = self.verifying.read();

            while walker.height < max_height {
                let entries = chain
                    .db
                    .get_next_path(&best, walker.height, MAX_BLOCKS_PER_PEER);

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
                    if chain.db.is_main_chain(entry) {
                        common.hash = Some(entry.hash);
                        common.height = Some(entry.height);
                        continue;
                    }

                    if verifying.contains(&entry.hash) {
                        continue;
                    }

                    // We have this block already.
                    if chain.db.has_block(entry.hash)? {
                        saved.push(**entry);
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

                walker = entries.back().expect("non empty checked above");
            }
        }

        if !saved.is_empty() {
            self.attach_block(saved[0]);
        }

        if items.is_empty() {
            return Ok(staller);
        }

        self.get_block(peer, items);

        Ok(staller)
    }

    fn attach_block(&self, entry: ChainEntry) {
        self.chain.write().attach(entry);
    }

    fn get_block(&self, peer: &Peer, hashes: Vec<BlockHash>) {
        let mut items = vec![];
        let now = util::now();

        {
            let chain = self.chain.read();
            let mut peer_block_map = peer.block_map.lock();
            let mut block_map = self.block_map.write();
            let verifying = self.verifying.read();

            for hash in hashes {
                if block_map.contains_key(&hash)
                    || chain.db.has_block(hash).unwrap()
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
        ));

        peer.block_time
            .store(util::now() as usize, Ordering::SeqCst);
    }

    fn handle_block(&self, peer: &Peer, block: Block) -> EmptyResult {
        let hash = block.block_hash();

        peer.block_map.lock().remove(&hash);

        let was_requested = self.block_map.write().remove(&hash).is_some();

        if !was_requested {
            trace!("destroy: block {} not requested {} ", hash, peer.addr);

            peer.destroy.store(true, Ordering::SeqCst);
            return Ok(());
        }

        peer.block_time
            .store(util::now() as usize, Ordering::SeqCst);

        peer.stalling_since.store(0, Ordering::SeqCst);

        let mut chain = self.chain.write();

        chain.add(block).unwrap();
        self.queue_resolve_headers();

        Ok(())
    }

    fn handle_get_headers(
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
            let chain = self.chain.read();

            let hash = if locator.is_empty() {
                Some(BlockHash::default())
            } else {
                let common = chain.find_locator(&locator);
                chain.db.get_next_hash(common)?
            };

            let mut entry = if let Some(hash) = hash {
                chain.db.get_entry_by_hash(&hash)
            } else {
                None
            };

            while let Some(e) = entry {
                headers.push(e.into());

                if let Some(stop) = stop {
                    if e.hash == stop {
                        break;
                    }
                }

                if headers.len() == 2000 {
                    break;
                }

                entry = chain.db.get_next(&e)?;
            }
        }

        peer.send(NetworkMessage::Headers(headers));

        Ok(())
    }

    fn is_syncable(&self, peer: &Peer) -> bool {
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
            let chain = self.chain.read();
            let peers = self.peers.read();
            if peers.loader != Some(peer.id) && !chain.is_recent() {
                return false;
            }
        }

        true
    }

    fn send_sync(&self, peer: &Peer) -> EmptyResult {
        if peer.syncing.load(Ordering::SeqCst) {
            return Ok(());
        }

        if !self.is_syncable(peer) {
            return Ok(());
        }

        peer.syncing.store(true, Ordering::SeqCst);

        let locator = {
            let chain = self.chain.read();
            let best = chain.most_work();
            let prev = chain.db.get_entry_by_hash(&best.prev_block);
            chain.get_locator(prev.map(|p| p.hash))
        };

        // let c = self.chain.read().get_locator(None);
        use std::str::FromStr;
        peer.send_get_headers(
            vec![BlockHash::from_str(
                "0000000000000000000a74566ea3048c3acd4b90a200a991070799b3696c0eb6",
            )
            .unwrap()],
            None,
        );

        Ok(())
    }
}

impl P2P {
    pub fn handle_packet(&self, peer: &Peer, packet: NetworkMessage) {
        let res = match packet {
            NetworkMessage::Headers(headers) => self.handle_headers(peer, headers),
            // NetworkMessage::Block(block) => self.handle_block(peer, block),
            // NetworkMessage::GetHeaders(msg) => {
            //     self.handle_get_headers(peer, msg.locator_hashes, msg.stop_hash)
            //         .await
            // }
            _ => Ok(()),
        };
        if let Err(err) = res {
            error!("destroy: error handling packet from {} {}", peer.addr, err);
            peer.destroy.store(true, Ordering::SeqCst);
        }
    }

    pub fn handle_close(&self, peer: &Peer, _connected: bool) {
        debug!("disconnected from {}", peer.addr);

        {
            let mut peers = self.peers.write();
            peers.remove(peer.id);
            if peers.loader == Some(peer.id) {
                peers.loader = None;
            }
        }

        {
            let mut peer_block_map = peer.block_map.lock();
            let mut block_map = self.block_map.write();

            for (hash, _) in peer_block_map.drain() {
                block_map.remove(&hash);
            }
        }

        self.queue_resolve_headers();
    }

    pub fn handle_open(&self, peer: &Peer) {
        debug!("handshake complete with {}", peer.addr);
        peer.send(NetworkMessage::SendHeaders);
        if self.send_sync(peer).is_err() {
            trace!("destroy: send sync error {} ", peer.addr);

            peer.destroy.store(true, Ordering::SeqCst);
        }
        self.queue_resolve_headers();
    }

    pub fn handle_error(&self, peer: &Peer, error: &Error) {
        debug!("error: {} from peer {}", error, peer.addr);
    }

    pub fn handle_ban(&self, _peer: &Peer) {}

    pub fn handle_connect(&self, peer: &Peer) {
        debug!("connected to {}", peer.addr);
    }
}
