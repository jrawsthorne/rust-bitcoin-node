use crate::protocol::consensus;
use crate::CoinView;
use bitcoin::Transaction;
use failure::{bail, ensure, Error};

pub trait TransactionVerifier {
    fn check_inputs(&self, view: &CoinView, height: u32) -> Result<u64, Error>;
    fn get_output_value(&self) -> u64;
    fn is_final(&self, height: u32, time: u32) -> bool;
    fn get_sigops_cost(&self, view: &CoinView) -> usize;
}

impl TransactionVerifier for Transaction {
    fn check_inputs(&self, view: &CoinView, height: u32) -> Result<u64, Error> {
        let mut total = 0;

        for input in &self.input {
            let coin = match view.get_entry(&input.previous_output) {
                Some(coin) => coin,
                None => bail!("bad-txns-inputs-missingorspent"),
            };

            ensure!(!coin.spent);

            if coin.coinbase {
                ensure!(height - coin.height.unwrap() >= consensus::COINBASE_MATURITY);
            }

            total += coin.output.value;

            if coin.output.value > consensus::MAX_MONEY || total > consensus::MAX_MONEY {
                bail!("bad-txns-inputvalues-outofrange");
            }
        }

        let value = self.get_output_value();

        if total < value {
            bail!("bad-txns-in-belowout");
        }

        let fee = total - value;

        if fee > consensus::MAX_MONEY {
            bail!("bad-txns-fee-outofrange");
        }

        Ok(total)
    }

    fn get_output_value(&self) -> u64 {
        self.output
            .iter()
            .fold(0, |total, output| total + output.value)
    }

    fn is_final(&self, height: u32, time: u32) -> bool {
        if self.lock_time == 0 {
            return true;
        }

        let lock_time = if self.lock_time < consensus::LOCKTIME_THRESHOLD {
            height
        } else {
            time
        };

        if self.lock_time < lock_time {
            return true;
        }

        for input in &self.input {
            if input.sequence != 0xffffffff {
                return false;
            }
        }

        true
    }

    fn get_sigops_cost(&self, _view: &CoinView) -> usize {
        todo!("sigops")
    }
}
