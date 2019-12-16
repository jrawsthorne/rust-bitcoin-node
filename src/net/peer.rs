use super::connection::ConnectionListener;
use bitcoin::network::message::NetworkMessage;
use failure::Error;

/// Unique ID for a connected peer
pub type PeerId = usize;

/// Trait that handles peer events
pub trait PeerListener {
    fn handle_open(&self, peer: &Peer) {}
    fn handle_close(&self, peer: &Peer, connected: bool) {}
    fn handle_packet(&self, peer: &Peer, packet: &NetworkMessage) {}
    fn handle_error(&self, peer: &Peer, error: &Error) {}
    fn handle_ban(&self, peer: &Peer) {}
    fn handle_connect(&self, peer: &Peer) {}
}

/// A remote node to send and receive P2P network messages
pub struct Peer {}

impl ConnectionListener for Peer {
    fn handle_connect(&self) {}
    fn handle_close(&self) {}
    fn handle_packet(&self, packet: NetworkMessage) {}
    fn handle_error(&self, error: &Error) {}
}
