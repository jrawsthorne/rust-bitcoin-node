use super::Peer;
use crate::blockchain::ChainEntry;
use bitcoin::{network::message::NetworkMessage, Block, Transaction};
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
