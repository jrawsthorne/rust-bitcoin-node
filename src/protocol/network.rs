use bitcoin::{consensus::params::Params, network::constants::Network, util::uint::Uint256};

#[derive(Clone)]
pub struct NetworkParams {
    pub network: Network,
    pub last_checkpoint: u32,
    pub halving_interval: u32,
    pub pow_limit: Uint256,
    pub dns_seeds: Vec<&'static str>,
}

impl Default for NetworkParams {
    fn default() -> Self {
        NetworkParams::from_network(Network::Bitcoin)
    }
}

impl NetworkParams {
    pub fn from_network(network: Network) -> Self {
        match network {
            Network::Bitcoin => Self {
                network,
                last_checkpoint: 525_000,
                halving_interval: 210_000,
                pow_limit: Params::new(Network::Bitcoin).pow_limit,
                dns_seeds: vec![
                    "seed.bitcoin.sipa.be",          // Pieter Wuille
                    "dnsseed.bluematt.me",           // Matt Corallo
                    "dnsseed.bitcoin.dashjr.org",    // Luke Dashjr
                    "seed.bitcoinstats.com",         // Christian Decker
                    "seed.bitcoin.jonasschnelli.ch", // Jonas Schnelli
                    "seed.btc.petertodd.org",        // Peter Todd
                ],
            },
            Network::Regtest => Self {
                network,
                last_checkpoint: 0,
                halving_interval: 210_000,
                pow_limit: Params::new(network).pow_limit,
                dns_seeds: vec![],
            },
            _ => todo!("testnet"),
        }
    }
}
