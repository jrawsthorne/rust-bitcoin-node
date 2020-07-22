use super::BlockTemplate;
use crate::blockchain::{Chain, ChainEntry};

use bitcoin::{
    util::hash::bitcoin_merkle_root, Address, Block, BlockHeader, Network, PublicKey, Transaction,
};
use log::debug;
use std::time::UNIX_EPOCH;

pub struct Miner {
    network: Network,
}

impl Default for Miner {
    fn default() -> Self {
        Self::new()
    }
}

impl Miner {
    pub fn new() -> Self {
        Self {
            network: Network::Regtest,
        }
    }

    pub fn create_block(
        &self,
        tip: ChainEntry,
        address: Option<Address>,
        chain: &Chain,
    ) -> BlockTemplate {
        let version = 0;
        let address = match address {
            Some(address) => address,
            None => self.get_address(),
        };
        let time = std::cmp::max(
            UNIX_EPOCH.elapsed().unwrap().as_secs() as u32,
            chain.get_median_time(&tip) + 1,
        );
        let target = chain.get_target(time, Some(&tip));

        let attempt = BlockTemplate::new(tip.height + 1, address, tip.hash, version, time, target);
        debug!(
            "Created block template (height={}, miner address={}).",
            attempt.height, attempt.address
        );
        attempt
    }

    // single threaded miner for creating test blocks
    pub fn mine_block(template: BlockTemplate, mut transactions: Vec<Transaction>) -> Block {
        let coinbase = template.create_coinbase();

        transactions.insert(0, coinbase);

        let hashes = transactions.iter().map(|obj| obj.txid().as_hash());
        let mr = bitcoin_merkle_root(hashes).into();

        let mut block_header = BlockHeader {
            version: template.version,
            prev_blockhash: template.prev_blockhash,
            merkle_root: mr,
            time: template.time,
            bits: template.target,
            nonce: 0,
        };

        while block_header.validate_pow(&block_header.target()).is_err() {
            block_header.nonce += 1;
        }

        Block {
            header: block_header,
            txdata: transactions,
        }
    }

    pub fn get_address(&self) -> Address {
        use bitcoin::secp256k1::{rand::thread_rng, Secp256k1};

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
