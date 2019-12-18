use super::connection::ConnectionListener;
use bitcoin::network::message::NetworkMessage;
use failure::Error;

/// Unique ID for a connected peer
pub type PeerId = usize;

/// Trait that handles peer events
pub trait PeerListener {
    fn handle_open(&self, _peer: &Peer) {}
    fn handle_close(&self, _peer: &Peer, _connected: bool) {}
    fn handle_packet(&self, _peer: &Peer, _packet: &NetworkMessage) {}
    fn handle_error(&self, _peer: &Peer, _error: &Error) {}
    fn handle_ban(&self, _peer: &Peer) {}
    fn handle_connect(&self, _peer: &Peer) {}
}

/// A remote node to send and receive P2P network messages
pub struct Peer {}

impl ConnectionListener for Peer {}
