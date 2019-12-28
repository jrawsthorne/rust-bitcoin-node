use super::NetworkParams;

/// 1 Bitcoin is 100 million satoshis
pub const COIN: u64 = 100_000_000;
/// The initial block subsidy, 50 Bitcoin
pub const BASE_REWARD: u64 = 50 * COIN;
/// The maximum money supply, 21 million Bitcoin
const MAX_MONEY: u64 = 21_000_000 * COIN;

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
            sum += subsidy * 1000;
            assert!(sum >= 0 && sum <= MAX_MONEY); // TODO: change value to be negative
        }
        assert_eq!(sum, 2_099_999_997_690_000); // Max Bitcoin money supply if all subsidies were claimed (they haven't been)
    }

    fn interval_params(interval: u32) -> NetworkParams {
        let mut params = NetworkParams::default();
        params.halving_interval = interval;
        params
    }
}
