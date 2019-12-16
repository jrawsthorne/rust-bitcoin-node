use bitcoin::network::message::NetworkMessage;
use failure::Error;

/// Trait that handles TCP stream events
pub trait ConnectionListener {
    fn handle_connect(&self) {}
    fn handle_close(&self) {}
    fn handle_packet(&self, packet: NetworkMessage) {}
    fn handle_error(&self, error: &Error) {}
}
