use super::BlockTemplate;
use crate::blockchain::ChainEntry;

use bitcoin::{consensus::params::Params, Address, Block, BlockHeader, Network, PublicKey};
use log::debug;
use std::time::UNIX_EPOCH;

pub struct Miner {
    network: Network,
}

impl Miner {
    pub fn new() -> Self {
        Self {
            network: Network::Regtest,
        }
    }

    pub fn create_block(&self, tip: ChainEntry, address: Option<Address>) -> BlockTemplate {
        let version = 0;
        let address = match address {
            Some(address) => address,
            None => self.get_address(),
        };
        let time = UNIX_EPOCH.elapsed().unwrap().as_secs() as u32;
        let target = Params::new(self.network).pow_limit;

        let attempt = BlockTemplate::new(tip.height + 1, address, tip.hash, version, time, target);
        debug!(
            "Created block template (height={}, miner address={}).",
            attempt.height, attempt.address
        );
        attempt
    }

    // single threaded miner for creating test blocks
    pub fn mine_block(template: BlockTemplate) -> Block {
        let coinbase = template.create_coinbase();
        let bits = BlockHeader::compact_target_from_u256(&template.target);
        let mut block_header = BlockHeader {
            version: template.version,
            prev_blockhash: template.prev_blockhash,
            merkle_root: coinbase.txid(),
            time: template.time,
            bits,
            nonce: 0,
        };

        while block_header.validate_pow(&block_header.target()).is_err() {
            block_header.nonce += 1;
        }

        Block {
            header: block_header,
            txdata: vec![coinbase],
        }
    }

    fn get_address(&self) -> Address {
        use bitcoin::secp256k1::Secp256k1;
        use rand::thread_rng;

        let secp = Secp256k1::new();

        let mut rng = thread_rng();
        let (_, public_key) = secp.generate_keypair(&mut rng);

        Address::p2pkh(
            &PublicKey {
                key: public_key,
                compressed: true,
            },
            self.network,
        )
    }
}
