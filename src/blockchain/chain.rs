use super::ChainEntry;
use crate::coins::CoinView;
use crate::net::PeerId;
use bitcoin::{hashes::sha256d::Hash as H256, Block};
use failure::Error;

/// Trait that handles chain events
pub trait ChainListener {
    /// Called when a new block is added to the main chain
    fn handle_connect(&self, _entry: &ChainEntry, _block: &Block, _view: &CoinView) {}
    /// Called when a block is removed from the main chain. This happens
    /// during a reorg or manual reindex
    fn handle_disconnect(&self, _entry: &ChainEntry, _block: &Block, _view: &CoinView) {}
    /// Called when a block cannot be added to the main chain because the
    /// previous block is not the tip of the main chain
    fn handle_orphan(&self, _block: &Block) {}
    /// Called when a valid block is recieved
    fn handle_block(&self, _block: &Block, _entry: &ChainEntry) {}
    /// Called when the chain is reset to a certain block
    fn handle_reset(&self, _tip: &ChainEntry) {}
    /// Called when the chain is fully synced
    fn handle_full(&self) {}
    /// Called when an orphan block couldn't be connected
    fn handle_bad_orphan(&self, _error: &Error, _peer_id: &PeerId) {}
    /// Called when the blockchain tip changes
    fn handle_tip(&self, _tip: &ChainEntry) {}
    /// Called when a competing chain with higher chainwork is received
    fn handle_reorg(&self, _tip: &ChainEntry, _competitor: &ChainEntry) {}
    /// Called when blocks from a competing chain are connected to the main chain
    fn handle_reconnect(&self, _entry: &ChainEntry, _block: &Block) {}
    /// Called when a block from an alternate chain is received.
    /// An alternate chain is one with the same of less chainwork than the main chain that both have
    fn handle_competitor(&self, _block: &Block, _entry: &ChainEntry) {}

    fn handle_resolved(&self, _block: &Block, _entry: &ChainEntry) {}
    fn handle_checkpoint(&self, _hash: H256, _height: u32) {}
}
