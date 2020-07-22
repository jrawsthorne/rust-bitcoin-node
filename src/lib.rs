//! Bitcoin full node written in Rust

/// Block verification
pub mod blockchain;
/// Flat file block storage
pub mod blockstore;
/// Utilities for wokring with coins (UTXO set)
pub mod coins;
/// Store blockchain state
pub mod db;
/// Custom Errors
pub mod error;
/// Utility to combine all functionality
mod full_node;
/// Compact Block Filter, Transaction and Address indexers
pub mod indexers;
/// Manages a pool of transactions yet to be included in a block
pub mod mempool;
/// Basic CPU miner for use in tests
pub mod mining;
/// Send messages to and received messages from the Bitcoin P2P network
pub mod net;
/// Extensions to rust-bitcoin primitives
pub mod primitives;
/// Protocol details
pub mod protocol;
/// Utilities
pub mod util;

pub use blockchain::ChainEntry;
pub use coins::{CoinEntry, CoinView};
pub use full_node::{Config, FullNode};
