use super::TransactionVerifier;
use crate::util::EmptyResult;
use bitcoin::Block;
use failure::{bail, ensure, Error};

pub trait BlockVerifier {
    fn get_claimed(&self) -> Result<u64, Error>;
    fn validate_pow(&self) -> EmptyResult;
    fn check_body(&self) -> EmptyResult;
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
        todo!("non contextual block verification");
    }
}
