use bitcoin::network::constants::Network;

pub struct NetworkParams {
    pub network: Network,
    pub last_checkpoint: u32,
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
            },
            _ => todo!("regtest and testnet"),
        }
    }
}
