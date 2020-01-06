use crate::coins::CoinView;
use crate::net::PeerId;
use bitcoin::{Block, Transaction};
use failure::Error;

/// Trait that handles mempool events
pub trait MempoolListener {
    fn handle_tx(&self, _tx: &Transaction, _view: &CoinView) {}
    fn handle_bad_orphan(&self, _error: &Error, _peer_id: PeerId) {}
    fn handle_confirmed(&self, _tx: &Transaction, _block: &Block) {}
    fn handle_error(&self, _error: &Error) {}
    fn handle_unconfirmed(&self, _tx: &Transaction, _block: &Block) {}
    fn handle_conflict(&self, _tx: &Transaction) {}
    fn handle_add_entry(&self, _entry: &MempoolEntry) {}
    fn handle_remove_entry(&self, _entry: &MempoolEntry) {}
    fn handle_add_orphan(&self, _tx: &Transaction) {}
    fn handle_remove_orphan(&self, _tx: &Transaction) {}
    fn handle_double_spend(&self, _spent: &MempoolEntry) {}
}

pub struct MempoolEntry {}
