use crate::protocol::{get_block_subsidy, NetworkParams};
use bitcoin::{blockdata::script, Address, BlockHash, Transaction, TxIn, TxOut};

pub struct BlockTemplate {
    pub height: u32,
    pub address: Address,
    network: NetworkParams,
    fees: u64,
    pub prev_blockhash: BlockHash,
    pub version: u32,
    pub time: u32,
    pub target: u32,
}

impl BlockTemplate {
    pub fn new(
        height: u32,
        address: Address,
        prev_blockhash: BlockHash,
        version: u32,
        time: u32,
        target: u32,
    ) -> Self {
        let mut network_params = NetworkParams::default();
        network_params.network = bitcoin::Network::Regtest;
        Self {
            height,
            address,
            network: network_params,
            fees: 0,
            prev_blockhash,
            version,
            time,
            target,
        }
    }

    pub fn create_coinbase(&self) -> Transaction {
        let mut coinbase = Transaction {
            version: 1,
            lock_time: 0,
            input: vec![],
            output: vec![],
        };

        let mut input = TxIn::default();
        input.script_sig = script::Builder::new()
            .push_int(self.height as i64)
            .push_slice(&[0]) // cb script sig padding (2 <= len <= 100)
            .into_script();
        coinbase.input.push(input);

        let output = TxOut {
            value: self.get_reward(),
            script_pubkey: self.address.script_pubkey(),
        };
        coinbase.output.push(output);

        coinbase
    }

    fn get_reward(&self) -> u64 {
        get_block_subsidy(self.height, &self.network) + self.fees
    }
}
