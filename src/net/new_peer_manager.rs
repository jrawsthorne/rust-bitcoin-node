use super::{
    connection::{DisconnectReceiver, MessageReceiver},
    new_peer::Peer,
};
use crate::{blockchain::Chain, protocol::NetworkParams, util, ChainEntry};
use anyhow::{anyhow, bail, Result};
use bitcoin::{
    network::{message::NetworkMessage, message_blockdata::Inventory},
    Block, BlockHash, BlockHeader,
};
use log::{debug, info, trace, warn};
use parking_lot::{Mutex, RwLock};
use std::{
    collections::HashMap, collections::HashSet, net::SocketAddr, sync::mpsc, sync::Arc,
    time::Duration,
};
use tokio::{
    net::{lookup_host, TcpStream},
    sync::{oneshot, Notify},
    time::{sleep, timeout},
};

const BLOCK_DOWNLOAD_WINDOW: usize = 1024;
const MAX_BLOCKS_PER_PEER: usize = 16;

pub struct PeerManager {
    max_outbound: usize,
    network_params: NetworkParams,
    state: Mutex<State>,
    chain: RwLock<Chain>,
    resolve_headers: Notify,
    add_blocks_tx: Mutex<mpsc::Sender<AddBlocksEvent>>,
}
#[derive(Debug, Default)]
pub struct State {
    peers: HashMap<SocketAddr, Arc<Peer>>,
    header_sync_peer: Option<Arc<Peer>>,
    requested_blocks: HashMap<BlockHash, SocketAddr>,
    verifying: HashSet<BlockHash>,
}

impl State {
    pub fn is_header_sync_peer(&self, peer: PeerRef) -> bool {
        match &self.header_sync_peer {
            Some(header_sync_peer) => header_sync_peer.addr == peer.addr,
            None => false,
        }
    }
}

pub type PeerRef<'a> = &'a Arc<Peer>;

impl PeerManager {
    pub fn new(
        max_outbound: usize,
        network_params: NetworkParams,
        chain: RwLock<Chain>,
    ) -> Arc<Self> {
        let (add_blocks_tx, add_blocks_rx) = mpsc::channel();

        let peer_manager = Arc::new(Self {
            max_outbound,
            network_params,
            state: Mutex::new(State::default()),
            chain,
            resolve_headers: Notify::new(),
            add_blocks_tx: Mutex::new(add_blocks_tx),
        });

        tokio::spawn(maintain_peers(peer_manager.clone()));
        tokio::spawn(resolve_headers(peer_manager.clone()));
        tokio::spawn(disconnect_stalling_peers(peer_manager.clone()));
        {
            let peer_manager = peer_manager.clone();
            std::thread::spawn(move || add_blocks(peer_manager, add_blocks_rx));
        }

        peer_manager
    }

    pub fn handshake_complete(&self, peer: PeerRef) {
        peer.queue_message(NetworkMessage::GetAddr);

        let mut state = self.state.lock();

        if state.header_sync_peer.is_none() {
            state.header_sync_peer.replace(peer.clone());
            drop(state);
            self.send_sync(peer);
        }

        self.queue_resolve_headers();
    }

    pub fn peer_disconnected(&self, peer: Arc<Peer>) {
        let mut state = self.state.lock();

        state.peers.remove(&peer.addr);

        {
            // remove any in flight blocks from requested set
            let mut peer_state = peer.state.lock();
            for (hash, _) in peer_state.requested_blocks.drain() {
                state.requested_blocks.remove(&hash);
            }
        }

        // if header sync peer, remove
        if state.is_header_sync_peer(&peer) {
            state.header_sync_peer.take();
        }

        // if no header sync peer, choose random from connected peers and send get headers
        if state.header_sync_peer.is_none() && !state.peers.is_empty() {
            let mut peers: Vec<&Arc<Peer>> = state.peers.values().collect();

            let mut rng = rand::thread_rng();
            use rand::prelude::*;
            peers.shuffle(&mut rng);

            let peer = peers[0].clone();

            state.header_sync_peer = Some(peer.clone());

            drop(state);
            self.send_sync(&peer);
        }

        self.queue_resolve_headers();
    }

    fn send_sync(&self, peer: PeerRef) {
        if peer.state.lock().syncing {
            return;
        }

        if !self.is_syncable(peer) {
            return;
        }

        peer.state.lock().syncing = true;

        let locator = {
            let chain = self.chain.read();
            let best = chain.most_work();
            let prev = chain.db.get_entry_by_hash(&best.prev_block);
            chain.get_locator(prev.map(|p| p.hash))
        };

        peer.get_headers(locator, None);
    }

    fn is_syncable(&self, peer: PeerRef) -> bool {
        let state = self.state.lock();
        let peer_state = peer.state.lock();

        if peer_state.disconnected {
            return false;
        }

        if !peer_state.handshake {
            return false;
        }

        {
            let chain = self.chain.read();
            if !chain.is_recent() && !state.is_header_sync_peer(&peer) {
                return false;
            }
        }

        true
    }

    async fn resolve_headers_for_peer<'a>(&self, peer: PeerRef<'a>) {
        if !self.is_syncable(peer) {
            return;
        }

        let best = {
            let peer_state = peer.state.lock();
            if !peer_state.requested_blocks.is_empty() {
                return;
            }
            match &peer_state.best_hash {
                None => {
                    drop(peer_state);
                    return self.send_sync(peer);
                }
                Some(best_hash) => {
                    let chain = self.chain.read();

                    let best = if let Some(&best) = chain.db.get_entry_by_hash(best_hash) {
                        best
                    } else {
                        return;
                    };

                    if best.chainwork <= chain.tip.chainwork {
                        return;
                    }

                    best
                }
            }
        };

        let staller = self.get_next_blocks(peer, best).await;

        if let Some(staller) = staller {
            let state = self.state.lock();
            let slow = state.peers.get(&staller);
            if let Some(slow) = slow {
                let mut peer_state = slow.state.lock();
                let now = util::now();

                if let Some(stalling_since) = peer_state.stalling_since {
                    if now > stalling_since + 2 {
                        drop(peer_state);
                        slow.disconnect(Err(anyhow!("Peer is stalling block download window")))
                    }
                } else {
                    peer_state.stalling_since.replace(now);
                }
            }
        }
    }

    async fn get_next_blocks(&self, peer: &Peer, best: ChainEntry) -> Option<SocketAddr> {
        let mut items = vec![];
        let mut saved = vec![];
        let mut waiting = None;
        let mut staller = None;

        {
            let state = self.state.lock();
            let mut peer_state = peer.state.lock();
            let chain = self.chain.read();

            if peer_state.common_hash.is_none() {
                let common_height = std::cmp::min(chain.height, best.height);
                let common_entry = chain
                    .db
                    .get_entry_by_height(common_height)
                    .expect("height is at most chain height so entry must exist");
                peer_state.common_height = Some(common_height);
                peer_state.common_hash = Some(common_entry.hash);
            }

            let mut walker = chain
                .db
                .get_entry_by_hash(&peer_state.common_hash.expect("common hash set above"))
                .expect("common hash so must have that hash");

            walker = chain
                .common_ancestor(&walker, &best)
                .expect("have block in common so must be an ancestor");

            peer_state.common_hash = Some(walker.hash);
            peer_state.common_height = Some(walker.height);

            let end_height = peer_state.common_height.expect("set when common hash set") as usize
                + BLOCK_DOWNLOAD_WINDOW;
            let max_height = std::cmp::min(best.height, (end_height as u32) + 1);

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
                    if state.requested_blocks.contains_key(&entry.hash) {
                        if waiting.is_none() {
                            waiting = state.requested_blocks.get(&entry.hash).copied();
                        }
                        continue;
                    }

                    // The block is considered invalid.
                    if chain.db.has_invalid(&entry.hash) {
                        return staller;
                    }

                    // We have verified this block already (even if pruned).
                    if chain.db.is_main_chain(entry) {
                        peer_state.common_hash = Some(entry.hash);
                        peer_state.common_height = Some(entry.height);
                        continue;
                    }

                    if state.verifying.contains(&entry.hash) {
                        continue;
                    }

                    // We have this block already.
                    if chain.db.has_block(entry.hash) {
                        saved.push(**entry);
                        continue;
                    }

                    // We have reached the end of the window.
                    if entry.height > end_height as u32 {
                        // We could add to the request if the first in-flight
                        // block were to complete (or the window was one larger).
                        // Identify the peer with the first in-flight block as
                        // the slowest and stalling the download.
                        if items.is_empty() && waiting != Some(peer.addr) {
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
            self.attach_block(saved[0]).await;
        }

        if items.is_empty() {
            return staller;
        }

        self.get_block(peer, items);

        staller
    }

    // TODO: Handle errors
    async fn attach_block(&self, entry: ChainEntry) {
        let (tx, rx) = oneshot::channel();
        self.add_blocks_tx
            .lock()
            .send(AddBlocksEvent::Attach(entry, tx))
            .expect("thread panicked");
        rx.await.expect("thread panicked");
    }

    fn get_block(&self, peer: &Peer, hashes: Vec<BlockHash>) {
        let mut items = vec![];
        let now = util::now();

        let mut state = self.state.lock();
        let mut peer_state = peer.state.lock();

        // TODO: Does this fix stalling peer issue?
        if peer_state.disconnected {
            return;
        }

        let chain = self.chain.read();

        for hash in hashes {
            if state.verifying.contains(&hash)
                || state.requested_blocks.contains_key(&hash)
                || chain.db.has_block(hash)
            {
                continue;
            }

            state.requested_blocks.insert(hash, peer.addr);
            peer_state.requested_blocks.insert(hash, now);

            items.push(hash);
        }

        if items.is_empty() {
            return;
        }

        debug!(
            "Requesting {}/{} blocks from peer with getdata ({}).",
            items.len(),
            state.requested_blocks.len(),
            peer.addr
        );

        peer.queue_message(NetworkMessage::GetData(
            items.into_iter().map(Inventory::WitnessBlock).collect(),
        ));

        peer_state.block_time = Some(util::now());
    }

    fn queue_resolve_headers(&self) {
        self.resolve_headers.notify_one();
    }

    async fn resolve_headers(&self) {
        let peers = {
            if !self.chain.read().is_recent() {
                return;
            }

            let state = self.state.lock();

            if state.requested_blocks.len() > BLOCK_DOWNLOAD_WINDOW - MAX_BLOCKS_PER_PEER {
                return;
            }

            let peers: Vec<_> = state.peers.values().cloned().collect();

            peers
        };

        for peer in peers {
            self.resolve_headers_for_peer(&peer).await;
        }
    }
}

/// Message Handlers
impl PeerManager {
    pub async fn handle_message<'a>(
        &self,
        message: NetworkMessage,
        peer: PeerRef<'a>,
    ) -> Result<()> {
        match message {
            NetworkMessage::Headers(headers) => self.handle_headers(peer, headers)?,
            NetworkMessage::Block(block) => self.handle_block(peer, block).await?,
            _ => {}
        }

        Ok(())
    }

    pub fn handle_headers(&self, peer: PeerRef, headers: Vec<BlockHeader>) -> Result<()> {
        if headers.is_empty() {
            return Ok(());
        }

        let mut last: Option<&BlockHeader> = None;
        for header in &headers {
            if last.is_some() && header.prev_blockhash != last.unwrap().block_hash() {
                bail!("bad header chain");
            }
            last = Some(header);
        }

        {
            let mut peer_state = peer.state.lock();
            let mut chain = self.chain.write();

            for header in &headers {
                let hash = header.block_hash();

                if chain.db.has_header(&hash) {
                    // We don't need these headers, but now we know this peer's tip hash
                    // Without a ChainEntry though, we don't know their height.
                    peer_state.best_hash = Some(hash);
                    trace!("already have header {} ({})", hash, peer.addr);
                    continue;
                }

                let entry = match chain.add_header(&header) {
                    Ok((entry, _prev)) => entry,
                    Err(_err) => {
                        bail!("bad header");
                    }
                };

                // Keep track of peer's best hash.
                peer_state.best_hash = Some(hash);

                // Keep track of peer's best height.
                peer_state.best_height = Some(entry.height);
            }
        }

        debug!("Received {} headers from peer {}", headers.len(), peer.addr);

        if headers.len() == 2000 {
            let locator = self
                .chain
                .read()
                .get_locator(Some(headers[headers.len() - 1].block_hash()));
            peer.get_headers(locator, None);
        }

        self.queue_resolve_headers();

        Ok(())
    }

    pub async fn handle_block<'a>(&self, peer: PeerRef<'a>, block: Block) -> Result<()> {
        let hash = block.block_hash();

        {
            let mut state = self.state.lock();
            let mut peer_state = peer.state.lock();

            let was_requested = peer_state.requested_blocks.remove(&hash).is_some();

            if !was_requested {
                bail!("unrequested block");
            }

            state.requested_blocks.remove(&hash).unwrap();

            peer_state.block_time = Some(util::now());

            peer_state.stalling_since = None;

            assert!(
                state.verifying.insert(hash),
                "FIX: requested duplicate blocks"
            );
        }

        let (tx, rx) = oneshot::channel::<Result<ChainEntry>>();
        let _ = self
            .add_blocks_tx
            .lock()
            .send(AddBlocksEvent::AddBlock(block, tx));
        rx.await.unwrap()?;

        self.state.lock().verifying.remove(&hash);

        self.queue_resolve_headers();

        Ok(())
    }
}

async fn maintain_peers(peer_manager: Arc<PeerManager>) {
    let mut addrs = vec![];
    let mut seen = HashSet::new();

    'main: loop {
        let needed = {
            let n_peers = peer_manager.state.lock().peers.len();
            peer_manager.max_outbound.saturating_sub(n_peers)
        };

        for _ in 0..needed {
            if let Some(addr) = addrs.pop() {
                tokio::spawn(connect_to_peer(addr, peer_manager.clone()));
            } else {
                let seeds = &peer_manager.network_params.dns_seeds;
                let port = &peer_manager.network_params.p2p_port;

                for seed in seeds {
                    info!("fetching addrs from {}", seed);
                    if let Ok(seed_addrs) = lookup_host(format!("{}:{}", seed, port)).await {
                        addrs.extend(seed_addrs.into_iter().filter(|addr| seen.insert(*addr)));
                    }
                }

                use rand::prelude::*;
                let mut rng = rand::thread_rng();
                addrs.shuffle(&mut rng);

                continue 'main;
            }
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn connect_to_peer(addr: SocketAddr, peer_manager: Arc<PeerManager>) {
    info!("connecting to {}", addr);
    let stream = timeout(Duration::from_secs(10), TcpStream::connect(addr)).await;

    if let Ok(Ok(stream)) = stream {
        info!("connected to {}", addr);

        // broadcast messages right away rather than waiting for timeout
        let _ = stream.set_nodelay(true);

        let (peer, mut message_rx, disconnect_rx) =
            Peer::new(addr, stream, peer_manager.network_params.network);

        let peer = Arc::new(peer);

        peer_manager.state.lock().peers.insert(addr, peer.clone());

        match timeout(Duration::from_secs(10), peer.handshake(&mut message_rx)).await {
            Ok(Err(error)) => {
                warn!(
                    "error during handshake, disconnecting: {} ({})",
                    error, addr
                );
                peer_manager.state.lock().peers.remove(&addr);
                return;
            }
            Err(_) => {
                warn!("error during handshake, disconnecting: timeout ({})", addr);
                peer_manager.state.lock().peers.remove(&addr);
                return;
            }
            _ => {
                peer_manager.handshake_complete(&peer);

                run_peer(peer, message_rx, disconnect_rx, peer_manager).await;
            }
        }
    } else {
        warn!("error connecting to {}", addr);
    }
}

// TODO: Clean up

async fn run_peer(
    peer: Arc<Peer>,
    mut message_rx: MessageReceiver,
    mut disconnect_rx: DisconnectReceiver,
    peer_manager: Arc<PeerManager>,
) {
    loop {
        tokio::select! {
            message = message_rx.recv() => {
                if let Some(message) = message {
                    if let Err(error) = peer.handle_message(&message) {
                        warn!(
                            "error handling message, disconnecting: {} ({})",
                            error, peer.addr
                        );
                        break;
                    }
                    tokio::select! {
                        result = peer_manager.handle_message(message, &peer) => {
                            if let Err(error) = result {
                                warn!(
                                    "error handling message, disconnecting: {} ({})",
                                    error, peer.addr
                                );
                                break;
                            }
                        }
                        result = disconnect_rx.recv() => {
                            if let Some(result) = result {
                                if let Err(error) = result {
                                    warn!("disconnected with error: {} ({})", error, peer.addr);
                                } else {
                                    warn!("disconnected cleanly ({})", peer.addr);
                                }
                            }
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
            result = disconnect_rx.recv() => {
                if let Some(result) = result {
                    if let Err(error) = result {
                        warn!("disconnected with error: {} ({})", error, peer.addr);
                    } else {
                        warn!("disconnected cleanly ({})", peer.addr);
                    }
                }
                break;
            }
        }
    }

    peer.state.lock().disconnected = true;

    peer_manager.peer_disconnected(peer);
}

async fn resolve_headers(peer_manager: Arc<PeerManager>) {
    loop {
        peer_manager.resolve_headers.notified().await;
        peer_manager.resolve_headers().await;
    }
}

async fn disconnect_stalling_peers(peer_manager: Arc<PeerManager>) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let state = peer_manager.state.lock();
        for peer in state.peers.values() {
            let peer_state = peer.state.lock();

            let now = util::now();

            if peer_state.syncing
                && peer_state.requested_blocks.len() > 0
                && matches!(peer_state.block_time, Some(block_time) if now > block_time + 120)
            {
                drop(peer_state);
                peer.disconnect(Err(anyhow!("Peer is stalling (block)")));
                continue;
            }

            if let Some(stalling_since) = peer_state.stalling_since {
                if now > stalling_since + 2 {
                    drop(peer_state);
                    peer.disconnect(Err(anyhow!("Peer is stalling block download window")));
                    continue;
                }
            }
        }
    }
}

enum AddBlocksEvent {
    AddBlock(Block, oneshot::Sender<Result<ChainEntry>>),
    Attach(ChainEntry, oneshot::Sender<()>),
}

fn add_blocks(peer_manager: Arc<PeerManager>, rx: mpsc::Receiver<AddBlocksEvent>) {
    loop {
        while let Ok(event) = rx.recv() {
            match event {
                AddBlocksEvent::AddBlock(block, tx) => {
                    let mut chain = peer_manager.chain.write();

                    let res = chain
                        .add(block)
                        .map_err(|err| anyhow!("peer sent invalid block: {}", err));

                    let _ = tx.send(res);
                }
                AddBlocksEvent::Attach(entry, tx) => {
                    let mut chain = peer_manager.chain.write();
                    chain.attach(entry).expect("todo");
                    peer_manager.queue_resolve_headers();
                    let _ = tx.send(());
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use bitcoin::Network;
    use tokio::task::yield_now;

    use crate::blockchain::ChainOptions;

    use super::*;

    #[tokio::test]
    async fn test_disconnect() {
        env_logger::builder().format_timestamp_millis().init();

        let network_params = NetworkParams::from_network(Network::Bitcoin);

        let chain = RwLock::new(
            Chain::new(ChainOptions {
                network: network_params.clone(),
                verify_scripts: true,
                path: "./data".into(),
            })
            .unwrap(),
        );

        let peer_manager = PeerManager::new(8, network_params, chain);

        sleep(Duration::from_secs(10)).await;

        let mut addrs = vec![];

        {
            let state = peer_manager.state.lock();

            for peer in state.peers.values() {
                addrs.push(peer.addr);
                peer.disconnect(Ok(()));
            }
        }

        loop {
            {
                let state = peer_manager.state.lock();

                if addrs.iter().all(|addr| !state.peers.contains_key(addr)) {
                    break;
                }
            }

            yield_now().await;
        }
    }
}
