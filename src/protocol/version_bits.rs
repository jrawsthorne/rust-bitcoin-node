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
    pub start_time: StartTime,
    pub timeout: Timeout,
    pub name: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum StartTime {
    AlwaysActive,
    StartTime(u32),
}

#[derive(Debug, Clone, Copy)]
pub enum Timeout {
    NoTimeout,
    Timeout(u32),
}

impl BIP9Deployment {
    pub fn new(name: &'static str, bit: u8, start_time: StartTime, timeout: Timeout) -> Self {
        Self {
            bit,
            start_time,
            timeout,
            name,
        }
    }

    pub fn always_active(&self) -> bool {
        match &self.start_time {
            StartTime::AlwaysActive => true,
            _ => false,
        }
    }

    pub fn no_timeout(&self) -> bool {
        match self.timeout {
            Timeout::NoTimeout => true,
            _ => false,
        }
    }
}
