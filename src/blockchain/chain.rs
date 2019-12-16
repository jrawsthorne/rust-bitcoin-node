use super::ChainEntry;
use crate::coins::CoinView;
use crate::net::PeerId;
use bitcoin::{hashes::sha256d::Hash as H256, Block};
use failure::Error;

/// Trait that handles chain events
pub trait ChainListener {
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
