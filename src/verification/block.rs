use super::TransactionVerifier;
use crate::protocol::consensus::*;
use crate::util::EmptyResult;
use bitcoin::{
    blockdata::constants::*, blockdata::script::Builder, consensus::encode::VarInt, Block,
};
use failure::{bail, ensure, Error};

pub trait BlockVerifier {
    fn get_claimed(&self) -> Result<u64, Error>;
    fn validate_pow(&self) -> EmptyResult;
    fn check_body(&self) -> EmptyResult;
    fn get_weight(&self) -> usize;
    fn check_coinbase_height(&self, height: u32) -> EmptyResult;
    fn has_witness(&self) -> bool;
    fn get_size(&self) -> usize;
}

impl BlockVerifier for Block {
    fn get_claimed(&self) -> Result<u64, Error> {
        let cb = match self.txdata.get(0) {
            Some(cb) => cb,
            None => bail!("no coinbase"),
        };
        ensure!(cb.is_coin_base());
        Ok(cb.get_output_value())
    }

    fn validate_pow(&self) -> Result<(), Error> {
        self.header.validate_pow(&self.header.target())?;
        Ok(())
    }

    fn check_body(&self) -> EmptyResult {
        // Check the merkle root.
        // TODO: check mutated
        ensure!(self.check_merkle_root(), "bad-txnmrklroot");

        // Size limits

        ensure!(!self.txdata.is_empty(), "bad-blk-length");
        ensure!(
            self.txdata.len() * WITNESS_SCALE_FACTOR <= MAX_BLOCK_WEIGHT as usize,
            "bad-blk-length"
        );
        ensure!(
            self.get_size() * WITNESS_SCALE_FACTOR <= MAX_BLOCK_WEIGHT as usize,
            "bad-blk-length"
        );

        // First transaction must be coinbase
        ensure!(self.txdata[0].is_coin_base(), "bad-cb-missing");

        // Check transactions

        let mut sigops = 0;
        for (index, tx) in self.txdata.iter().enumerate() {
            if index > 0 && tx.is_coin_base() {
                bail!("bad-cb-multiple")
            }

            tx.check_sanity()?;

            sigops += tx.get_legacy_sig_op_count();
        }

        ensure!(
            sigops * WITNESS_SCALE_FACTOR <= MAX_BLOCK_SIGOPS_COST,
            "bad-blk-sigops"
        );

        Ok(())
    }

    fn get_weight(&self) -> usize {
        let header = 80;
        let tx_len = VarInt(self.txdata.len() as u64).len();
        let tx = self
            .txdata
            .iter()
            .fold(0, |total, tx| total + tx.get_weight());

        header + tx_len + tx
    }

    fn check_coinbase_height(&self, height: u32) -> EmptyResult {
        ensure!(self.header.version >= 2);

        ensure!(!self.txdata.is_empty());

        let coinbase = &self.txdata[0];

        ensure!(!coinbase.input.is_empty());

        let coinbase_script = &coinbase.input[0].script_sig;

        let expected = Builder::new().push_scriptint(height as i64).into_script();

        if expected[..] == coinbase_script[0..expected.len()] {
            Ok(())
        } else {
            bail!("bad-cb-height")
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

    fn get_size(&self) -> usize {
        let header = 80;
        let tx_len = VarInt(self.txdata.len() as u64).len();
        let tx = self
            .txdata
            .iter()
            .fold(0, |total, tx| total + tx.get_size());

        header + tx_len + tx
    }
}
