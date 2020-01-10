use crate::protocol::{get_block_subsidy, NetworkParams};
use bitcoin::util::uint::Uint256;
use bitcoin::{blockdata::script, Address, BlockHash, BlockHeader, Transaction, TxIn, TxOut};

pub struct BlockTemplate {
    pub height: u32,
    pub address: Address,
    network: NetworkParams,
    fees: u64,
    pub prev_blockhash: BlockHash,
    pub version: u32,
    pub time: u32,
    pub bits: u32,
    pub target: Uint256,
}

impl BlockTemplate {
    pub fn new(
        height: u32,
        address: Address,
        prev_blockhash: BlockHash,
        version: u32,
        time: u32,
        target: Uint256,
    ) -> Self {
        let mut network_params = NetworkParams::default();
        network_params.network = bitcoin::Network::Regtest;
        let bits = BlockHeader::compact_target_from_u256(&target);
        Self {
            height,
            address,
            network: network_params,
            fees: 0,
            prev_blockhash,
            version,
            time,
            bits,
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
