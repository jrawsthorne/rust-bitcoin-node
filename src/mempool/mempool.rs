use crate::coins::CoinView;
use crate::net::PeerId;
use bitcoin::{Block, Transaction};
use failure::Error;

/// Trait that handles mempool events
pub trait MempoolListener {
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

pub struct MempoolEntry {}
