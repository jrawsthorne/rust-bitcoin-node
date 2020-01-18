use bitcoin::{
    consensus::params::Params, network::constants::Network, util::uint::Uint256, BlockHeader,
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
    /// Determines whether retargeting is disabled for this network or not.
    pub no_pow_retargeting: bool,
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
            ..
        } = Params::new(network);
        let pow_limit_bits = BlockHeader::compact_target_from_u256(&pow_limit);
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
                no_pow_retargeting,
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
                no_pow_retargeting,
            },
            _ => todo!("testnet"),
        }
    }
}
