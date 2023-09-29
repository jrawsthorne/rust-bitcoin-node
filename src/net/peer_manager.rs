use super::{
    peer::{self, PeerRef},
    CompactBlock, Peer,
};
use crate::{
    blockchain::Chain,
    indexers::FilterIndexer,
    mempool::MemPool,
    protocol::NetworkParams,
    // util::now,
};
use anyhow::{bail, Result};
use bitcoin::{
    hashes::sha256d,
    network::{
        // constants::ServiceFlags,
        message::NetworkMessage,
        message_blockdata::{GetHeadersMessage, Inventory},
        message_compact_blocks::{BlockTxn, CmpctBlock, GetBlockTxn},
        message_filter::{CFHeaders, CFilter, GetCFHeaders, GetCFilters},
        message_network::VersionMessage,
        Address,
    },
    util::bip152::{BlockTransactions, BlockTransactionsRequest, HeaderAndShortIds},
    Block, BlockHash, BlockHeader, MerkleBlock, Transaction, Txid, Wtxid,
};

use log::{debug, info, trace, warn};
use parking_lot::RwLock;
use peer::TxRelay;
use std::{
    collections::{HashMap, HashSet},
    net::TcpStream,
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

// fn required_services() -> ServiceFlags {
//     ServiceFlags::WITNESS | ServiceFlags::NETWORK
// }

pub struct PeerManager {
    network: NetworkParams,
    pub state: RwLock<State>,
    pub chain: Arc<RwLock<Chain>>,
    mempool: Option<Arc<RwLock<MemPool>>>,
    filter_index: Option<Arc<RwLock<FilterIndexer>>>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct GenericTxid {
    pub hash: sha256d::Hash,
    pub wtxid: bool,
}

impl std::fmt::Display for GenericTxid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.hash.fmt(f)
    }
}

impl From<Txid> for GenericTxid {
    fn from(txid: Txid) -> Self {
        Self {
            hash: txid.as_hash(),
            wtxid: false,
        }
    }
}

impl From<Wtxid> for GenericTxid {
    fn from(wtxid: Wtxid) -> Self {
        Self {
            hash: wtxid.as_hash(),
            wtxid: true,
        }
    }
}

impl From<GenericTxid> for Txid {
    fn from(gtxid: GenericTxid) -> Txid {
        gtxid.hash.into()
    }
}

#[derive(Default)]
pub struct State {
    pub peers: HashMap<SocketAddr, Arc<Peer>>,
    pub header_sync_peer: Option<Arc<Peer>>,
    tx_map: HashSet<GenericTxid>,
    block_map: HashSet<BlockHash>,
    verifying: HashSet<sha256d::Hash>,
    compact_blocks: HashSet<BlockHash>,
}

pub fn maintain_peers(peer_manager: Arc<PeerManager>) {
    let seeds = &peer_manager.network.dns_seeds;
    let mut addrs = vec![];

    let populate_seeds = |addrs: &mut Vec<SocketAddr>| {
        for seed in seeds {
            info!("fetching addrs from {}", seed);
            if let Ok(seed_addrs) = (*seed, peer_manager.network.p2p_port).to_socket_addrs() {
                addrs.extend(seed_addrs);
            }
        }
        use rand::prelude::*;
        let mut rng = rand::thread_rng();
        addrs.shuffle(&mut rng);
    };

    loop {
        let removed = {
            let state = &mut *peer_manager.state.write();
            let block_map = &mut state.block_map;
            let header_sync_peer = &mut state.header_sync_peer;
            let mut removed = false;

            state.peers.retain(|_, peer| {
                let mut peer_state = peer.state.write();
                if peer_state.disconnect {
                    for hash in peer_state.block_map.drain() {
                        block_map.remove(&hash);
                    }

                    if let Some(hsp) = header_sync_peer {
                        if std::ptr::eq(peer, hsp) {
                            header_sync_peer.take();
                        }
                    }

                    removed = true;
                    false
                } else {
                    true
                }
            });

            removed
        };

        if removed {
            peer_manager.get_next_blocks().unwrap();
        }

        while peer_manager.state.read().peers.len() < 8 {
            if let Some(addr) = addrs.pop() {
                info!("connecting to {}", addr);
                if let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
                    let peer = Peer::new_outbound(
                        stream,
                        peer_manager.clone(),
                        peer_manager.network.network,
                    );
                    peer_manager.state.write().peers.insert(addr, peer.clone());
                    if peer.send_version().is_err() {
                        peer.state.write().disconnect = true;
                    }
                }
            } else {
                populate_seeds(&mut addrs);
            }
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

impl PeerManager {
    pub fn new(
        chain: Arc<RwLock<Chain>>,
        mempool: Option<Arc<RwLock<MemPool>>>,
        filter_index: Option<Arc<RwLock<FilterIndexer>>>,
        network: NetworkParams,
    ) -> Arc<PeerManager> {
        Arc::new(PeerManager {
            state: Default::default(),
            chain,
            mempool,
            filter_index,
            network,
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
        // let mut state = peer.state.write();

        // let services = required_services();
        // let now = now() as u32;

        let count = addrs.len();

        // for (mut time, addr) in addrs {
        //     state.addr_filter.insert(addr.clone());

        //     if !addr.services.has(services) {
        //         continue;
        //     }

        //     if time <= 100000000 || time > now + 10 * 60 {
        //         time = now - 5 * 24 * 60 * 60;
        //     }

        //     if addr.port == 0 {
        //         continue;
        //     }

        //     // TODO: Add to seen addresses
        // }

        info!(
            "Received {} addrs (hosts={}, peers={}) ({}).",
            count,
            0,
            self.state.read().peers.len(),
            peer.addr
        );
    }

    pub fn handle_inv(&self, peer: PeerRef, items: Vec<Inventory>) -> Result<()> {
        if !self.chain.read().synced() {
            return Ok(());
        }

        let mut txids = vec![];

        let mut peer_state = peer.state.write();
        let mut state = self.state.write();

        for item in items {
            let gtxid = match item {
                Inventory::Transaction(txid) => Some(txid.into()),
                Inventory::WTx(wtxid) => Some(wtxid.into()),
                _ => None,
            };
            if let Some(gtxid) = gtxid {
                if state.tx_map.contains(&gtxid) {
                    continue;
                }

                peer_state.inv_filter.insert(gtxid.hash);
                state.tx_map.insert(gtxid);
                peer_state.tx_map.insert(gtxid);

                txids.push(gtxid);
            }
        }

        if !txids.is_empty() {
            debug!(
                "Requesting {}/{} txs from peer with getdata ({}).",
                txids.len(),
                state.tx_map.len(),
                peer.addr
            );
            peer.get_transactions(txids)?;
        }
        Ok(())
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

    pub fn handle_get_data(&self, peer: PeerRef, items: Vec<Inventory>) -> Result<()> {
        if items.len() > 50_000 {
            bail!("inv too big");
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

                        peer.send(NetworkMessage::Tx(tx))?;

                        txs += 1;
                    } else {
                        not_found.push(item);
                    }
                }
                Inventory::WitnessBlock(hash) => {
                    let block = self.chain.read().db.get_block(hash).unwrap();
                    if let Some(block) = block {
                        peer.send(NetworkMessage::Block(block))?;
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
                                peer.send(NetworkMessage::Block(block))?;
                                blocks += 1;
                            } else {
                                not_found.push(item);
                            }
                        } else {
                            let block = self.chain.read().db.get_block(hash).unwrap();
                            if let Some(block) = block {
                                peer.send_compact_block(&block)?;

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

                        peer.send(NetworkMessage::MerkleBlock(merkle_block))?;

                        for tx in matches {
                            peer.send(NetworkMessage::Tx(tx))?;
                            txs += 1;
                        }
                    } else {
                        not_found.push(item);
                    }
                }
                Inventory::Error => {}
                Inventory::Unknown { .. } => {}
                Inventory::Transaction(_) => {}
                Inventory::FilteredBlock(_) => {}
                Inventory::Block(_) => {}
                Inventory::WTx(_) => {}
            }
        }

        let not_found_count = not_found.len();

        if !not_found.is_empty() {
            peer.send(NetworkMessage::NotFound(not_found))?;
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

        Ok(())
    }

    pub fn handle_mempool(&self, peer: PeerRef) -> Result<()> {
        if let Some(mempool) = &self.mempool {
            let items = mempool
                .read()
                .transactions
                .keys()
                .map(|txid| Inventory::WitnessTransaction(*txid))
                .collect();
            peer.send(NetworkMessage::Inv(items))?;
        }
        Ok(())
    }

    pub fn handle_tx(&self, peer: PeerRef, tx: Transaction) {
        let gtxid = {
            // let mut state = self.state.write();
            let mut peer_state = peer.state.write();

            let gtxid = match peer_state.tx_relay {
                TxRelay::Txid => tx.txid().into(),
                TxRelay::Wtxid => tx.wtxid().into(),
            };

            if !peer_state.tx_map.remove(&gtxid) {
                return;
            }
            // state.tx_map.remove(&gtxid);

            gtxid
        };

        if let Some(mempool) = &self.mempool {
            let chain = self.chain.read();
            if let Err(err) = mempool.write().add_tx(&chain, tx) {
                warn!("{} couldn't be added to mempool: {}", gtxid, err);
            }
        }
    }

    pub fn handle_block(&self, peer: PeerRef, block: Block) -> Result<()> {
        let hash = block.block_hash();

        {
            let mut state = self.state.write();
            let mut peer_state = peer.state.write();

            if !peer_state.block_map.remove(&hash) {
                bail!("didn't ask for block");
            }

            assert!(state.block_map.remove(&hash));
            state.verifying.insert(hash.as_hash());
        }

        self.chain.write().add(block).unwrap();

        assert!(self.state.write().verifying.remove(&hash.as_hash()));

        self.get_next_blocks()
    }

    pub fn get_next_blocks(&self) -> Result<()> {
        let chain = self.chain.read();
        let state = &mut *self.state.write();

        for peer in state.peers.values() {
            let mut peer_state = peer.state.write();

            if !peer_state.ack {
                continue;
            }

            if chain.is_recent() && peer_state.block_map.is_empty() {
                let mut height = chain.tip.height;

                let mut hashes = vec![];

                while hashes.is_empty() && height <= chain.most_work().height {
                    let next = chain
                        .db
                        .get_next_path(chain.most_work(), height, 16)
                        .into_iter()
                        .filter_map(|e| {
                            if state.verifying.contains(&e.hash.as_hash())
                                || chain.db.has_block(e.hash)
                                || state.block_map.contains(&e.hash)
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

                drop(peer_state);

                peer.get_blocks(hashes)?;
            }
        }

        Ok(())
    }

    pub fn handle_get_headers(
        &self,
        peer: PeerRef,
        locator_hashes: Vec<BlockHash>,
        stop_hash: BlockHash,
    ) -> Result<()> {
        let chain = self.chain.read();

        if !chain.synced() {
            return Ok(());
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
            None => return Ok(()),
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

        peer.send_headers(headers)
    }

    pub fn handle_headers(&self, peer: PeerRef, headers: Vec<BlockHeader>) -> Result<()> {
        if headers.is_empty() {
            return Ok(());
        }

        let mut last: Option<&BlockHeader> = None;
        for header in &headers {
            if let Some(l) = last {
                if header.prev_blockhash != l.block_hash() {
                    warn!("Peer sent a bad header chain ({}).", peer.addr);
                    bail!("bad header chain");
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
                    bail!("bad header");
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
            peer.send_get_headers(locator_hashes, None)?;
        }

        drop(peer_state);
        drop(chain);

        self.get_next_blocks()
    }

    pub fn handle_compact_block(
        &self,
        peer: PeerRef,
        header_and_short_ids: HeaderAndShortIds,
    ) -> Result<()> {
        let mut peer_state = peer.state.write();
        let mut state = self.state.write();

        let header = &header_and_short_ids.header;
        let hash = header.block_hash();

        if peer_state.compact_blocks.contains_key(&hash) {
            debug!("Peer sent us a duplicate compact block ({}).", peer.addr);
            return Ok(());
        }

        if state.compact_blocks.contains(&hash) {
            debug!(
                "Already waiting for compact block {} ({}).",
                hash, peer.addr
            );
            return Ok(());
        }

        if !peer_state.block_map.contains(&hash) {
            peer_state.block_map.insert(hash);
            assert!(!state.block_map.contains(&hash));
            state.block_map.insert(hash);
        }

        if self.chain.write().add_header(header).is_err() {
            debug!("Peer sent an invalid compact block ({}).", peer.addr);
            bail!("invalid compact block");
        }

        let mut block = match CompactBlock::new(header_and_short_ids) {
            None => {
                warn!(
                    "Siphash collision for {}. Requesting full block ({}).",
                    hash, peer.addr
                );
                bail!("siphash collision");
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
            return self.handle_block(peer, block.into());
        }

        if peer_state.compact_blocks.len() >= 15 {
            warn!("Compact block DoS attempt ({}).", peer.addr);
            bail!("compact block dos");
        }

        debug!(
            "Received non-full compact block {} tx={}/{} ({}).",
            hash, block.count, block.total_tx, peer.addr
        );

        let txs_request = block.into_block_transactions_request();

        assert!(peer_state.compact_blocks.insert(hash, block).is_none());
        state.compact_blocks.insert(hash);

        peer.send(NetworkMessage::GetBlockTxn(GetBlockTxn { txs_request }))
    }

    pub fn handle_block_txn(
        &self,
        peer: PeerRef,
        transactions: Vec<Transaction>,
        hash: BlockHash,
    ) -> Result<()> {
        let mut state = self.state.write();
        let mut peer_state = peer.state.write();

        let mut block = match peer_state.compact_blocks.remove(&hash) {
            None => {
                debug!("Peer sent unsolicited blocktxn ({}).", peer.addr);
                return Ok(());
            }
            Some(block) => block,
        };

        assert!(state.compact_blocks.remove(&hash));

        if !block.fill_missing(transactions) {
            warn!(
                "Peer sent non-full blocktxn for {}. Requesting full block ({}).",
                hash, peer.addr
            );
            bail!("peer sent non full blocktxn");
        }

        debug!("Filled compact block {} ({})", hash, peer.addr);

        drop(state);
        drop(peer_state);

        self.handle_block(peer, block.into())
    }

    // Kicks everything off for this peer
    pub fn handle_verack(&self, peer: PeerRef) -> Result<()> {
        // let chain = self.chain.read();

        peer.send_prefer_headers()?;
        peer.send_compact(true)?;
        peer.send_get_addr()?;

        let mut state = self.state.write();
        let chain = self.chain.read();

        if peer.outbound {
            let ask_for_headers = {
                if state.header_sync_peer.is_none() {
                    state.header_sync_peer = Some(peer.clone());
                    true
                } else {
                    chain.synced()
                }
            };

            if ask_for_headers {
                let best = chain.most_work();
                let hash = chain.db.get_entry_by_hash(&best.prev_block).map(|e| e.hash);
                let locator_hashes = chain.get_locator(hash);

                peer.send_get_headers(locator_hashes, None)?;
            }
        }

        // TODO: Announce

        // TODO: Set new fee rate when synced

        // if !chain.synced() {
        //     peer.send_fee_rate(MAX_MONEY);
        // }

        // if chain.synced() {
        //     peer.send_mempool();
        // }

        Ok(())
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
                    Inventory::Error | Inventory::Unknown { .. } => continue,
                    Inventory::WTx(_) => todo!("wtxid transaction requesting"),
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
    ) -> Result<()> {
        if filter_type != 0 {
            bail!("bad filter type");
        }

        let filter_index = match &self.filter_index {
            None => return Ok(()),
            Some(filter_index) => filter_index.read(),
        };

        let chain = self.chain.read();

        let hashes = chain.height_to_hash_range(start_height, &stop_hash, 2000);

        let filter_hashes = filter_index.filter_hashes_by_block_hashes(&hashes);

        let previous_filter_header = if start_height > 0 {
            let hash = chain.db.get_entry_by_height(start_height - 1).unwrap().hash;
            filter_index.filter_header_by_hash(hash).unwrap()
        } else {
            Default::default()
        };

        peer.send(NetworkMessage::CFHeaders(CFHeaders {
            filter_hashes,
            previous_filter_header,
            stop_hash,
            filter_type,
        }))
    }

    pub fn handle_get_cfilters(
        &self,
        peer: PeerRef,
        filter_type: u8,
        start_height: u32,
        stop_hash: BlockHash,
    ) -> Result<()> {
        if filter_type != 0 {
            bail!("bad filter type");
        }

        let filter_index = match &self.filter_index {
            None => return Ok(()),
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
            }))?;
        }

        Ok(())
    }

    pub fn handle_get_block_txn(
        &self,
        peer: PeerRef,
        request: BlockTransactionsRequest,
    ) -> Result<()> {
        let block = self
            .chain
            .read()
            .db
            .get_block(BlockHash::from_hash(request.block_hash))
            .unwrap();
        if let Some(block) = block {
            if let Ok(transactions) = BlockTransactions::from_request(&request, &block) {
                peer.send(NetworkMessage::BlockTxn(BlockTxn { transactions }))?;
            }
        }
        Ok(())
    }

    pub fn handle_message(&self, peer: PeerRef, message: NetworkMessage) -> Result<()> {
        match message {
            NetworkMessage::Version(version) => {
                self.handle_version(peer, version);
            }

            NetworkMessage::Verack => {
                self.handle_verack(peer)?;
            }

            NetworkMessage::Addr(addrs) => {
                self.handle_addr(peer, addrs);
            }

            NetworkMessage::Inv(items) => {
                self.handle_inv(peer, items)?;
            }

            NetworkMessage::GetData(items) => {
                self.handle_get_data(peer, items)?;
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
                self.handle_get_headers(peer, locator_hashes, stop_hash)?;
            }

            NetworkMessage::MemPool => {
                self.handle_mempool(peer)?;
            }

            NetworkMessage::Tx(tx) => {
                self.handle_tx(peer, tx);
            }

            NetworkMessage::Block(block) => {
                self.handle_block(peer, block)?;
            }

            NetworkMessage::Headers(headers) => {
                self.handle_headers(peer, headers)?;
            }

            NetworkMessage::GetAddr => {
                self.handle_get_addr(peer);
            }

            NetworkMessage::GetCFilters(GetCFilters {
                filter_type,
                start_height,
                stop_hash,
            }) => {
                self.handle_get_cfilters(peer, filter_type, start_height, stop_hash)?;
            }

            NetworkMessage::GetCFHeaders(GetCFHeaders {
                filter_type,
                start_height,
                stop_hash,
            }) => {
                self.handle_get_cfheaders(peer, filter_type, start_height, stop_hash)?;
            }

            NetworkMessage::GetCFCheckpt(_) => {}

            NetworkMessage::CmpctBlock(CmpctBlock { compact_block }) => {
                self.handle_compact_block(peer, compact_block)?;
            }

            NetworkMessage::GetBlockTxn(GetBlockTxn { txs_request }) => {
                self.handle_get_block_txn(peer, txs_request)?;
            }

            NetworkMessage::BlockTxn(BlockTxn {
                transactions:
                    BlockTransactions {
                        block_hash,
                        transactions,
                    },
            }) => {
                self.handle_block_txn(peer, transactions, BlockHash::from_hash(block_hash))?;
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

            NetworkMessage::WtxidRelay => {}
            NetworkMessage::AddrV2(_) => {}
            NetworkMessage::SendAddrV2 => {}
            NetworkMessage::Unknown { .. } => {}
        }

        Ok(())
    }
}

fn resolve_item(state: &mut State, peer_state: &mut peer::State, item: &Inventory) -> bool {
    match item {
        Inventory::Error | Inventory::Unknown { .. } => false,
        &Inventory::Transaction(txid) | &Inventory::WitnessTransaction(txid) => {
            resolve_tx(state, peer_state, &txid.into())
        }
        Inventory::Block(hash)
        | Inventory::FilteredBlock(hash)
        | Inventory::CompactBlock(hash)
        | Inventory::WitnessBlock(hash)
        | Inventory::WitnessFilteredBlock(hash) => resolve_block(state, peer_state, hash),
        &Inventory::WTx(wtxid) => resolve_tx(state, peer_state, &wtxid.into()),
    }
}

fn resolve_block(state: &mut State, peer_state: &mut peer::State, hash: &BlockHash) -> bool {
    if !peer_state.block_map.remove(hash) {
        return false;
    }

    assert!(state.block_map.remove(hash));

    true
}

fn resolve_tx(state: &mut State, peer_state: &mut peer::State, gtxid: &GenericTxid) -> bool {
    if !peer_state.tx_map.remove(gtxid) {
        return false;
    }

    assert!(state.tx_map.remove(gtxid));

    true
}
