use super::{tx::tx_stripped_size, TransactionVerifier};
use crate::{
    error::{BlockHeaderVerificationError, BlockVerificationError},
    protocol::consensus::*,
};
use bitcoin::{
    blockdata::constants::*,
    blockdata::script::{read_scriptint, Instruction},
    Block, VarInt,
};

pub trait BlockVerifier {
    fn get_claimed(&self) -> u64;
    fn validate_pow(&self) -> Result<(), BlockHeaderVerificationError>;
    fn check_body(&self) -> Result<(), BlockVerificationError>;
    fn check_coinbase_height(&self, height: u32) -> Result<(), BlockVerificationError>;
    fn has_witness(&self) -> bool;
}

impl BlockVerifier for Block {
    fn get_claimed(&self) -> u64 {
        assert!(!self.txdata.is_empty());
        assert!(self.txdata[0].is_coin_base());
        self.txdata[0].get_output_value()
    }

    fn validate_pow(&self) -> Result<(), BlockHeaderVerificationError> {
        self.header
            .validate_pow(&self.header.target())
            .map_err(|_| BlockHeaderVerificationError::InvalidPOW)
    }

    fn check_body(&self) -> Result<(), BlockVerificationError> {
        // Check the merkle root.
        // TODO: check mutated
        if !self.check_merkle_root() {
            return Err(BlockVerificationError::BadMerkleRoot);
        }

        // Size limits

        if self.txdata.is_empty()
            || self.txdata.len() * WITNESS_SCALE_FACTOR > MAX_BLOCK_WEIGHT as usize
            || block_stripped_size(self) * WITNESS_SCALE_FACTOR > MAX_BLOCK_WEIGHT as usize
        {
            return Err(BlockVerificationError::BadLength);
        }

        // First transaction must be coinbase
        if !self.txdata[0].is_coin_base() {
            return Err(BlockVerificationError::NoCoinbase);
        }
        // Check transactions

        let mut sigops = 0;
        for (index, tx) in self.txdata.iter().enumerate() {
            if index > 0 && tx.is_coin_base() {
                return Err(BlockVerificationError::MultipleCoinbase);
            }

            tx.check_sanity()?;

            sigops += tx.get_legacy_sig_op_count();
        }

        if sigops * WITNESS_SCALE_FACTOR > MAX_BLOCK_SIGOPS_COST {
            return Err(BlockVerificationError::BadSigops);
        }

        Ok(())
    }

    fn check_coinbase_height(&self, height: u32) -> Result<(), BlockVerificationError> {
        use BlockVerificationError::BadCoinbaseHeight;

        if self.header.version < 2 {
            return Err(BadCoinbaseHeight);
        }

        if self.txdata.is_empty() {
            return Err(BadCoinbaseHeight);
        }

        let coinbase = &self.txdata[0];

        if coinbase.input.is_empty() {
            return Err(BadCoinbaseHeight);
        }

        let coinbase_script = &coinbase.input[0].script_sig;

        match coinbase_script.iter(false).next() {
            Some(Instruction::PushBytes(bytes)) => {
                let actual_height = read_scriptint(bytes).map_err(|_| BadCoinbaseHeight)?;
                if actual_height != height as i64 {
                    Err(BadCoinbaseHeight)
                } else {
                    Ok(())
                }
            }
            _ => Err(BadCoinbaseHeight),
        }
    }

    fn has_witness(&self) -> bool {
        for tx in &self.txdata {
            if tx.has_witness() {
                return true;
            }
        }
        false
    }
}

/// Block size without witness
pub fn block_stripped_size(block: &Block) -> usize {
    let base_size = 80 + VarInt(block.txdata.len() as u64).len();
    let txs_size: usize = block.txdata.iter().map(tx_stripped_size).sum();
    base_size + txs_size
}
