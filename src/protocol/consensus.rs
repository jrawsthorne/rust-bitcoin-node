use super::NetworkParams;

/// 1 Bitcoin is 100 million satoshis
pub const COIN: u64 = 100_000_000;
/// The initial block subsidy, 50 Bitcoin
pub const BASE_REWARD: u64 = 50 * COIN;
/// The maximum money supply, 21 million Bitcoin
pub const MAX_MONEY: u64 = 21_000_000 * COIN;
/// Number of confirmations before a coinbase transaction can be spent
pub const COINBASE_MATURITY: u32 = 100;
/// Threshold for nLockTime: below this value it is interpreted as block number,
/// otherwise as UNIX timestamp.
pub const LOCKTIME_THRESHOLD: u32 = 500_000_000;
pub const MAX_BLOCK_SIGOPS_COST: usize = 80000;
/// The maximum number of public keys per multisig
pub const MAX_PUBKEYS_PER_MULTISIG: usize = 20;
/// P2WPKH sighash
pub const WITNESS_V0_KEYHASH_SIZE: usize = 20;
/// P2WSH sighash
pub const WITNESS_V0_SCRIPTHASH_SIZE: usize = 32;
pub const MEDIAN_TIMESPAN: usize = 11;
pub const MAX_BLOCK_SIZE: usize = 1_000_000;
pub const MAX_FUTURE_BLOCK_TIME: u32 = 2 * 60 * 60;

pub const SEQUENCE_GRANULARITY: u32 = 9;
pub const SEQUENCE_DISABLE_FLAG: u32 = 1 << 31;
pub const SEQUENCE_TYPE_FLAG: u32 = 1 << 22;
pub const SEQUENCE_MASK: u32 = 0x0000ffff;
pub const BIP34_IMPLIES_BIP30_LIMIT: u32 = 1_983_702;
pub const MAX_SCRIPT_PUSH: usize = 520;

bitflags::bitflags! {
    #[derive(Debug, Default, Clone, Copy)]
    pub struct ScriptFlags: u32 {
        const VERIFY_P2SH = 1 << 0;
        const VERIFY_STRICTENC = 1 << 1;
        const VERIFY_DERSIG = 1 << 2;
        const VERIFY_LOW_S = 1 << 3;
        const VERIFY_NULLDUMMY = 1 << 4;
        const VERIFY_SIGPUSHONLY = 1 << 5;
        const VERIFY_MINIMALDATA = 1 << 6;
        const VERIFY_DISCOURAGE_UPGRADABLE_NOPS = 1 << 7;
        const VERIFY_CLEANSTACK = 1 << 8;
        const VERIFY_CHECKLOCKTIMEVERIFY = 1 << 9;
        const VERIFY_CHECKSEQUENCEVERIFY = 1 << 10;
        const VERIFY_WITNESS = 1 << 11;
        const VERIFY_DISCOURAGE_UPGRADABLE_WITNESS_PROGRAM = 1 << 12;
        const VERIFY_MINIMALIF = 1 << 13;
        const VERIFY_NULLFAIL = 1 << 14;
        const VERIFY_WITNESS_PUBKEYTYPE = 1 << 15;
        const VERIFY_CONST_SCRIPTCODE = 1 << 16;
        const STANDARD_VERIFY_FLAGS = Self::VERIFY_P2SH.bits()
        | Self::VERIFY_STRICTENC.bits()
        | Self::VERIFY_DERSIG.bits()
        | Self::VERIFY_LOW_S.bits()
        | Self::VERIFY_NULLDUMMY.bits()
        | Self::VERIFY_DISCOURAGE_UPGRADABLE_NOPS.bits()
        | Self::VERIFY_CLEANSTACK.bits()
        | Self::VERIFY_MINIMALIF.bits()
        | Self::VERIFY_NULLFAIL.bits()
        | Self::VERIFY_CHECKLOCKTIMEVERIFY.bits()
        | Self::VERIFY_CHECKSEQUENCEVERIFY.bits()
        | Self::VERIFY_WITNESS.bits()
        | Self::VERIFY_DISCOURAGE_UPGRADABLE_WITNESS_PROGRAM.bits()
        | Self::VERIFY_WITNESS_PUBKEYTYPE.bits()
        | Self::VERIFY_CONST_SCRIPTCODE.bits()
        | Self::VERIFY_MINIMALDATA.bits();
        // NOT YET STANDARD
        const VERIFY_TAPROOT = 1 << 17;
        const VERIFY_DISCOURAGE_UPGRADABLE_TAPROOT_VERSION = 1 << 18;
        const VERIFY_DISCOURAGE_UNKNOWN_ANNEX = 1 << 19;
        const VERIFY_DISCOURAGE_OP_SUCCESS = 1 << 20;
        const VERIFY_DISCOURAGE_UPGRADABLE_PUBKEYTYPE = 1 << 21;
        const VERIFY_STANDARD_TEMPLATE = 1 << 22;
    }
}

bitflags::bitflags! {
    #[derive(Debug, Default, Clone, Copy)]
    pub struct LockFlags: u32 {
        const VERIFY_SEQUENCE = 1 << 0;
        const MEDIAN_TIME_PAST = 1 << 1;
        const STANDARD_LOCKTIME_FLAGS = Self::VERIFY_SEQUENCE.bits() | Self::MEDIAN_TIME_PAST.bits();
    }
}

/// Get the correct miner subsidy for a block at a certain height
/// On the main bitcoin network this halves the subsidy every 210,000 blocks (~4 years)
pub fn get_block_subsidy(height: u32, network_params: &NetworkParams) -> u64 {
    let halvings = height / network_params.halving_interval;

    if halvings >= 64 {
        return 0;
    }

    BASE_REWARD >> halvings
}

#[cfg(test)]
mod test {

    // https://github.com/bitcoin/bitcoin/blob/46fc4d1a24c88e797d6080336e3828e45e39c3fd/src/test/validation_tests.cpp

    use super::*;
    use bitcoin::Network;

    fn test_block_subsidy_halvings(network_params: &NetworkParams) {
        let max_halvings = 64;
        let initial_subsidy = BASE_REWARD;
        let mut previous_subsidy = initial_subsidy * 2;

        assert_eq!(previous_subsidy, initial_subsidy * 2);

        for halvings in 0..max_halvings {
            let height = halvings * network_params.halving_interval;
            let subsidy = get_block_subsidy(height, network_params);
            assert!(subsidy <= initial_subsidy);
            assert_eq!(subsidy, previous_subsidy / 2);
            previous_subsidy = subsidy;
        }

        assert_eq!(
            get_block_subsidy(
                max_halvings * network_params.halving_interval,
                network_params
            ),
            0
        );
    }

    #[test]
    fn test_block_subsidy() {
        test_block_subsidy_halvings(&NetworkParams::from_network(Network::Bitcoin));
        test_block_subsidy_halvings(&interval_params(150));
        test_block_subsidy_halvings(&interval_params(1000));
    }

    #[test]
    fn test_subsidy_limit() {
        let network_params = NetworkParams::default();
        let mut sum = 0;
        for height in (0..14_000_000).step_by(1000) {
            let subsidy = get_block_subsidy(height, &network_params);
            assert!(subsidy <= BASE_REWARD);
            sum += subsidy.checked_mul(1000).unwrap();
            assert!(sum <= MAX_MONEY);
        }
        assert_eq!(sum, 2_099_999_997_690_000); // Max Bitcoin money supply if all subsidies were claimed (they haven't been)
    }

    fn interval_params(interval: u32) -> NetworkParams {
        NetworkParams {
            halving_interval: interval,
            ..Default::default()
        }
    }
}
