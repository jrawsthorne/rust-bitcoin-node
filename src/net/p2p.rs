use super::peer::{Peer, PeerListener};
use crate::blockchain::ChainEntry;
use crate::blockchain::ChainListener;
use crate::mempool::MempoolListener;
use bitcoin::{network::message::NetworkMessage, Block, Transaction};
use failure::Error;
use std::net::SocketAddr;

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

/// A collection of connected peers
pub struct P2P {}

impl MempoolListener for P2P {}

impl PeerListener for P2P {}

impl ChainListener for P2P {}
