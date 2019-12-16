use super::peer::{Peer, PeerId, PeerListener};
use crate::blockchain::ChainEntry;
use crate::blockchain::ChainListener;
use crate::coins::CoinView;
use crate::mempool::{MempoolEntry, MempoolListener};
use bitcoin::{
    hashes::sha256d::Hash as H256, network::message::NetworkMessage, Block, Transaction,
};
use failure::Error;
use std::net::SocketAddr;

/// Trait that handles P2P events
pub trait P2PListener {
    fn handle_tx(&self, tx: &Transaction) {}
    fn handle_error(&self, error: &Error) {}
    fn handle_connection(&self, addr: &SocketAddr) {}
    fn handle_listening(&self) {}
    fn handle_block(&self, block: &Block, entry: &ChainEntry) {}
    fn handle_full(&self) {}
    fn handle_loader(&self, peer: &Peer) {}
    fn handle_packet(&self, packet: &NetworkMessage, peer: &Peer) {}
    fn handle_peer_connect(&self, peer: &Peer) {}
    fn handle_peer_open(&self, peer: &Peer) {}
    fn handle_peer_close(&self, peer: &Peer, connected: bool) {}
    fn handle_ban(&self, peer: &Peer) {}
    fn handle_peer(&self, peer: &Peer) {}
    fn handle_timeout(&self) {}
    fn handle_ack(&self, peer: &Peer) {}
    fn handle_reject(&self, peer: &Peer) {}
    fn handle_open(&self) {}
}

/// A collection of connected peers
pub struct P2P {}

impl MempoolListener for P2P {
    fn handle_tx(&self, tx: &Transaction, view: &CoinView) {}
    fn handle_bad_orphan(&self, error: &Error, peer_id: &PeerId) {}
    fn handle_confirmed(&self, tx: &Transaction, block: &Block) {}
    fn handle_error(&self, error: &Error) {}
    fn handle_unconfirmed(&self, tx: &Transaction, block: &Block) {}
    fn handle_conflict(&self, tx: &Transaction) {}
    fn handle_add_entry(&self, entry: &MempoolEntry) {}
    fn handle_remove_entry(&self, entry: &MempoolEntry) {}
    fn handle_add_orphan(&self, tx: &Transaction) {}
    fn handle_remove_orphan(&self, tx: &Transaction) {}
    fn handle_double_spend(&self, spent: &MempoolEntry) {}
}

impl PeerListener for P2P {
    fn handle_open(&self, peer: &Peer) {}
    fn handle_close(&self, peer: &Peer, connected: bool) {}
    fn handle_packet(&self, peer: &Peer, packet: &NetworkMessage) {}
    fn handle_error(&self, peer: &Peer, error: &Error) {}
    fn handle_ban(&self, peer: &Peer) {}
    fn handle_connect(&self, peer: &Peer) {}
}

impl ChainListener for P2P {
    fn handle_connect(&self, entry: &ChainEntry, block: &Block, view: &CoinView) {}
    fn handle_disconnect(&self, entry: &ChainEntry, block: &Block, view: &CoinView) {}
    fn handle_orphan(&self, block: &Block) {}
    fn handle_block(&self, block: &Block, entry: &ChainEntry) {}
    fn handle_reset(&self, tip: &ChainEntry) {}
    fn handle_full(&self) {}
    fn handle_bad_orphan(&self, error: &Error, peer_id: &PeerId) {}
    fn handle_tip(&self, tip: &ChainEntry) {}
    fn handle_reorg(&self, tip: &ChainEntry, competitor: &ChainEntry) {}
    fn handle_reconnect(&self, entry: &ChainEntry, block: &Block) {}
    fn handle_competitor(&self, block: &Block, entry: &ChainEntry) {}
    fn handle_resolved(&self, block: &Block, entry: &ChainEntry) {}
    fn handle_checkpoint(&self, hash: H256, height: u32) {}
}
