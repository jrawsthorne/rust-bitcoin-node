use super::{BIP9Deployment, TimeData};
use bitcoin::{
    consensus::params::Params, network::constants::Network, util::uint::Uint256, BlockHeader,
};
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
    pub deployments: HashMap<&'static str, BIP9Deployment>,
    pub rule_change_activation_threshold: u32,
    pub miner_confirmation_window: u32,
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
        match network {
            Network::Bitcoin => Self {
                network,
                last_checkpoint: 525_000,
                halving_interval: 210_000,
                pow_limit,
                pow_limit_bits,
                dns_seeds: vec![
                    "seed.bitcoin.sipa.be",          // Pieter Wuille
                    "dnsseed.bluematt.me",           // Matt Corallo
                    "dnsseed.bitcoin.dashjr.org",    // Luke Dashjr
                    "seed.bitcoinstats.com",         // Christian Decker
                    "seed.bitcoin.jonasschnelli.ch", // Jonas Schnelli
                    "seed.btc.petertodd.org",        // Peter Todd
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
                time: time.clone(),
                deployments: vec![
                    (
                        "taproot",
                        BIP9Deployment::new("taproot", 2, 1199145601, 1230767999),
                    ),
                    (
                        "ctv",
                        // March 1, 2020 - March 1, 2021
                        BIP9Deployment::new("ctv", 5, 1583020800, 1614556800),
                    ),
                    (
                        "dummy",
                        BIP9Deployment::new("dummy", 28, 1199145601, 1230767999),
                    ),
                ]
                .into_iter()
                .collect::<HashMap<_, _>>(),
                rule_change_activation_threshold,
                miner_confirmation_window,
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
                time: time.clone(),
                deployments: vec![
                    (
                        "taproot",
                        BIP9Deployment::new("taproot", 2, 1199145601, 1230767999),
                    ),
                    ("ctv", BIP9Deployment::new("ctv", 5, 1199145601, 1230767999)),
                    (
                        "dummy",
                        BIP9Deployment::new("dummy", 28, 1199145601, 1230767999),
                    ),
                ]
                .into_iter()
                .collect::<HashMap<_, _>>(),
                rule_change_activation_threshold,
                miner_confirmation_window,
            },
            Network::Regtest => Self {
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
                time: time.clone(),
                deployments: vec![
                    (
                        "taproot",
                        BIP9Deployment::new(
                            "taproot",
                            2,
                            BIP9Deployment::ALWAYS_ACTIVE,
                            BIP9Deployment::NO_TIMEOUT,
                        ),
                    ),
                    (
                        "ctv",
                        BIP9Deployment::new(
                            "ctv",
                            2,
                            BIP9Deployment::ALWAYS_ACTIVE,
                            BIP9Deployment::NO_TIMEOUT,
                        ),
                    ),
                    (
                        "dummy",
                        BIP9Deployment::new("dummy", 28, 0, BIP9Deployment::NO_TIMEOUT),
                    ),
                ]
                .into_iter()
                .collect::<HashMap<_, _>>(),
                rule_change_activation_threshold,
                miner_confirmation_window,
            },
        }
    }
}
