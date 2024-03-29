use super::{Deployment, StartTime, TimeData, Timeout};
use bitcoin::{
    consensus::params::Params, network::constants::Network, util::uint::Uint256, BlockHash,
    BlockHeader,
};
use maplit::hashmap;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct NetworkParams {
    pub network: Network,
    pub last_checkpoint: u32,
    pub halving_interval: u32,
    pub pow_limit: Uint256,
    pub pow_limit_bits: u32,
    pub dns_seeds: Vec<&'static str>,
    /// Time when BIP16 becomes active.
    pub bip16_time: u32,
    /// Block height at which BIP34 becomes active.
    pub bip34_height: u32,
    /// Block height at which BIP65 becomes active.
    pub bip65_height: u32,
    /// Block height at which BIP66 becomes active.
    pub bip66_height: u32,
    /// Block height at which Segwit becomes active
    pub segwit_height: u32,
    /// Block height at which CSV becomes active
    pub csv_height: u32,
    /// Determines whether retargeting is disabled for this network or not.
    pub no_pow_retargeting: bool,
    pub p2p_port: u16,
    pub allow_min_difficulty_blocks: bool,
    pub pow_target_spacing: u32,
    pub max_tip_age: usize,
    pub time: Arc<Mutex<TimeData>>,
    pub deployments: HashMap<&'static str, Deployment>,
    pub rule_change_activation_threshold: u32,
    pub miner_confirmation_window: u32,
    pub bip30: HashMap<u32, BlockHash>,
    pub expected_tx_count: u64,
    pub expected_tx_height: u32,
}

impl Default for NetworkParams {
    fn default() -> Self {
        NetworkParams::from_network(Network::Bitcoin)
    }
}

impl NetworkParams {
    pub fn from_network(network: Network) -> Self {
        let Params {
            pow_limit,
            bip16_time,
            bip34_height,
            bip65_height,
            bip66_height,
            no_pow_retargeting,
            pow_target_spacing,
            allow_min_difficulty_blocks,
            rule_change_activation_threshold,
            miner_confirmation_window,
            ..
        } = Params::new(network);
        let pow_limit_bits = BlockHeader::compact_target_from_u256(&pow_limit);
        let pow_target_spacing = pow_target_spacing as u32;
        let time = Arc::new(Mutex::new(TimeData::default()));
        fn b(hash: &str) -> BlockHash {
            use std::str::FromStr;
            BlockHash::from_str(hash).unwrap()
        }
        match network {
            Network::Bitcoin => Self {
                network,
                last_checkpoint: 525_000,
                halving_interval: 210_000,
                pow_limit,
                pow_limit_bits,
                dns_seeds: vec![
                    "seed.bitcoin.sipa.be", // Pieter Wuille, only supports x1, x5, x9, and xd
                    "dnsseed.bluematt.me",  // Matt Corallo, only supports x9
                    "dnsseed.bitcoin.dashjr.org", // Luke Dashjr
                    "seed.bitcoinstats.com", // Christian Decker, supports x1 - xf
                    "seed.bitcoin.jonasschnelli.ch", // Jonas Schnelli, only supports x1, x5, x9, and xd
                    "seed.btc.petertodd.org",        // Peter Todd, only supports x1, x5, x9, and xd
                    "seed.bitcoin.sprovoost.nl",     // Sjors Provoost
                    "dnsseed.emzy.de",               // Stephan Oeste
                ],
                bip16_time,
                bip34_height,
                bip65_height,
                bip66_height,
                csv_height: 419328,
                segwit_height: 481824,
                no_pow_retargeting,
                p2p_port: 8333,
                allow_min_difficulty_blocks,
                pow_target_spacing,
                max_tip_age: 24 * 60 * 60,
                time,
                deployments: hashmap! {
                    "taproot"   => Deployment::new("taproot", 2, StartTime::StartTime(1619222400), Timeout::Timeout(1628640000), 709_632),
                    "dummy"     => Deployment::new("dummy", 28, StartTime::StartTime(1199145601), Timeout::Timeout(1230767999), 0)
                },
                rule_change_activation_threshold: 1815,
                miner_confirmation_window,
                bip30: hashmap! {
                    91842 => b("00000000000a4d0a398161ffc163c503763b1f4360639393e0e4c8e300e0caec"),
                    91880 => b("00000000000743f190a18c5577a3c2d2a1f610ae9601ac046a38084ccb7cd721")
                },
                expected_tx_count: 590_390_085, // 0000000000000000000749f392fbcea22a93939baaf481d1ec14bc9a084845f3
                expected_tx_height: 658_680,
            },
            Network::Testnet => Self {
                network,
                last_checkpoint: 0,
                halving_interval: 210_000,
                pow_limit,
                pow_limit_bits,
                dns_seeds: vec![
                    // "testnet-seed.bitcoin.jonasschnelli.ch",
                    "seed.tbtc.petertodd.org",
                    "seed.testnet.bitcoin.sprovoost.nl",
                    "testnet-seed.bluematt.me",
                ],
                bip16_time,
                bip34_height,
                bip65_height,
                bip66_height,
                csv_height: 770112,
                segwit_height: 834624,
                no_pow_retargeting,
                p2p_port: 18333,
                allow_min_difficulty_blocks,
                pow_target_spacing,
                max_tip_age: 24 * 60 * 60,
                time,
                deployments: hashmap! {
                    "taproot"   => Deployment::new("taproot", 2, StartTime::StartTime(1619222400), Timeout::Timeout(1628640000), 0),
                    "dummy"     => Deployment::new("dummy", 28, StartTime::StartTime(1199145601), Timeout::Timeout(1230767999), 0)
                },
                rule_change_activation_threshold,
                miner_confirmation_window,
                bip30: Default::default(),
                expected_tx_count: 58_401_978, // 0000000000000053b17c9df0accfaacbde3154283c237842f4b669debc8257e5
                expected_tx_height: 1_894_693,
            },
            Network::Regtest | Network::Signet => Self {
                network,
                last_checkpoint: 0,
                halving_interval: 210_000,
                pow_limit,
                pow_limit_bits,
                dns_seeds: vec![],
                bip16_time,
                bip34_height,
                bip65_height,
                bip66_height,
                csv_height: 432,
                segwit_height: 0,
                no_pow_retargeting,
                p2p_port: 48444,
                allow_min_difficulty_blocks,
                pow_target_spacing,
                max_tip_age: 0xffffffff,
                time,
                deployments: hashmap! {
                    "taproot"   => Deployment::new("taproot", 2, StartTime::AlwaysActive, Timeout::NoTimeout, 0),
                    "ctv"       => Deployment::new("ctv", 5, StartTime::AlwaysActive, Timeout::NoTimeout, 0),
                    "dummy"     => Deployment::new("dummy", 28, StartTime::StartTime(0), Timeout::NoTimeout, 0)
                },
                rule_change_activation_threshold: if matches!(network, Network::Signet) {
                    1815
                } else {
                    rule_change_activation_threshold
                },
                miner_confirmation_window,
                bip30: Default::default(),
                expected_tx_count: 0,
                expected_tx_height: 0,
            },
        }
    }
}
