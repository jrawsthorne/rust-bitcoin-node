pub const VERSIONBITS_TOP_BITS: u32 = 0x20000000;
pub const VERSIONBITS_TOP_MASK: u32 = 0xE0000000;
pub const VERSIONBITS_NUM_BITS: usize = 29;

// BIP 9 defines a finite-state-machine to deploy a softfork in multiple stages.
// State transitions happen during retarget period if conditions are met
// In case of reorg, transitions can go backward. Without transition, state is
// inherited between periods. All blocks of a period share the same state.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ThresholdState {
    // First state that each softfork starts out as. The genesis block is by definition in this state for each deployment.
    Defined,
    // For blocks past the starttime.
    Started,
    // For one retarget period after the first retarget period with STARTED blocks of which at least threshold have the associated bit set in nVersion.
    LockedIn,
    // For all blocks after the LOCKED_IN retarget period (final state)
    Active,
    // For all blocks once the first retarget period after the timeout time is hit, if LOCKED_IN wasn't already reached (final state)
    Failed,
}

#[derive(Debug, Copy, Clone)]
pub struct BIP9Deployment {
    pub bit: u8,
    pub start_time: i32,
    pub timeout: u32,
    pub name: &'static str,
}

impl BIP9Deployment {
    pub fn new(name: &'static str, bit: u8, start_time: i32, timeout: u32) -> Self {
        Self {
            bit,
            start_time,
            timeout,
            name,
        }
    }

    pub const NO_TIMEOUT: u32 = u32::max_value();
    pub const ALWAYS_ACTIVE: i32 = -1;
}
