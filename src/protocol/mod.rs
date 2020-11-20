pub mod consensus;
mod network;
mod timedata;
mod version_bits;

pub use consensus::{get_block_subsidy, LockFlags, ScriptFlags, BASE_REWARD, COIN};
pub use network::NetworkParams;
pub use timedata::TimeData;
pub use version_bits::{
    BIP8Deployment, BIP8ThresholdState, BIP9Deployment, BIP9ThresholdState, Deployment, StartTime,
    ThresholdState, Timeout, VERSIONBITS_NUM_BITS, VERSIONBITS_TOP_BITS, VERSIONBITS_TOP_MASK,
};

pub const WTXID_RELAY_VERSION: u32 = 70016;
