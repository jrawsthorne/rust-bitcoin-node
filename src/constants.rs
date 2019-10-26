pub const MIN_VERSION: u32 = 7001; // minimum p2p protocol version we support
pub const PING_INTERVAL: u64 = 30000; // How often to send a ping message (in ms)
pub const HANDSHAKE_TIMEOUT: u64 = 5000; // Timeout for each handshake message (version and verack) [in ms]
pub const USER_AGENT: &str = "/rust-bitcoin-node:0.1/";
pub const LOCAL_SERVICES: u64 = Services::NETWORK as u64 | Services::WITNESS as u64;
pub const CONNECT_TIMEOUT: u64 = 10000; // Timeout for trying to initially connect to peer
pub const DISCOVERY_INTERVAL: u64 = 10000; // Discovery interval for UPNP and DNS seeds
pub const MAX_INV: usize = 50000; // Maximum inv/getdata size

pub enum Services {
    NETWORK = 1,
    GETUTXO = 2,
    BLOOM = 4,
    WITNESS = 8,
    NETWORK_LIMITED = 1024,
}
