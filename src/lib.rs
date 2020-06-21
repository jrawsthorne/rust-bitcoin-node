//! Bitcoin full node written in Rust

/// Block verification
pub mod blockchain;
/// Flat file block storage
pub mod blockstore;
/// Utilities for wokring with coins (UTXO set)
pub mod coins;
/// Store blockchain state
pub mod db;
/// Manages a pool of transactions yet to be included in a block
pub mod mempool;
/// Basic CPU miner for use in tests
pub mod mining;
/// Send messages to and received messages from the Bitcoin P2P network
pub mod net;
/// Protocol details
pub mod protocol;
/// Utilities
pub mod util;
/// Verification
pub mod verification;

pub mod error;
pub use blockchain::{ChainEntry, ChainListener};

pub use blockchain::ChainEntry;
pub use coins::{CoinEntry, CoinView};
// pub use net::{Peer, PeerId, P2P};
