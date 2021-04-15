use bitcoin::consensus::{encode, Decodable, Encodable};

pub const VERSIONBITS_TOP_BITS: i32 = 0x20000000;
pub const VERSIONBITS_TOP_MASK: i32 = -536870912;

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

impl Deployment {
    pub fn bit(&self) -> u8 {
        self.bit
    }
}

impl ThresholdState {
    pub fn is_active(&self) -> bool {
        matches!(self, ThresholdState::Active)
    }

    pub fn is_defined(&self) -> bool {
        matches!(self, ThresholdState::Defined)
    }

    pub fn is_started(&self) -> bool {
        matches!(self, ThresholdState::Started)
    }

    pub fn is_locked_in(&self) -> bool {
        matches!(self, ThresholdState::LockedIn)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, ThresholdState::Failed)
    }
}

#[derive(Debug, Copy, Clone)]
pub struct Deployment {
    pub bit: u8,
    pub start_time: StartTime,
    pub timeout: Timeout,
    pub name: &'static str,
    pub min_activation_height: u32,
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

impl Deployment {
    pub fn new(
        name: &'static str,
        bit: u8,
        start_time: StartTime,
        timeout: Timeout,
        min_activation_height: u32,
    ) -> Self {
        Self {
            bit,
            start_time,
            timeout,
            name,
            min_activation_height,
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

impl Decodable for ThresholdState {
    fn consensus_decode<D: std::io::Read>(d: D) -> Result<Self, encode::Error> {
        let flag = u8::consensus_decode(d)?;
        Ok(match flag {
            0 => ThresholdState::Defined,
            1 => ThresholdState::Started,
            2 => ThresholdState::LockedIn,
            3 => ThresholdState::Active,
            4 => ThresholdState::Failed,
            _ => unreachable!(),
        })
    }
}

impl Encodable for ThresholdState {
    fn consensus_encode<W: std::io::Write>(&self, mut e: W) -> Result<usize, encode::Error> {
        let flag: u8 = match self {
            ThresholdState::Defined => 0,
            ThresholdState::Started => 1,
            ThresholdState::LockedIn => 2,
            ThresholdState::Active => 3,
            ThresholdState::Failed => 4,
        };
        flag.consensus_encode(&mut e)
    }
}
