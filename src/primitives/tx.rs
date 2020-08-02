use super::ScriptExt;
use crate::protocol::consensus::{self, ScriptFlags};
use crate::{error::TransactionVerificationError, CoinView};
use bitcoin::{blockdata::constants::*, consensus::Encodable, Transaction, VarInt};
use consensus::*;
use rayon::prelude::*;
use std::collections::HashSet;

pub trait TransactionExt {
    fn check_inputs(
        &self,
        view: &CoinView,
        height: u32,
    ) -> Result<u64, TransactionVerificationError>;
    fn get_output_value(&self) -> u64;
    fn is_final(&self, height: u32, time: i32) -> bool;
    fn get_sigop_cost(&self, view: &CoinView, flags: ScriptFlags) -> usize;
    fn get_legacy_sig_op_count(&self) -> usize;
    fn get_p2sh_sig_op_count(&self, view: &CoinView) -> usize;
    fn get_witness_sig_op_count(&self, view: &CoinView) -> usize;
    fn check_sanity(&self) -> Result<(), TransactionVerificationError>;
    fn has_witness(&self) -> bool;
    fn verify_scripts(
        &self,
        view: &CoinView,
        flags: &ScriptFlags,
    ) -> Result<(), TransactionVerificationError>;
    fn stripped_size(&self) -> usize;
    fn signals_replacement(&self) -> bool;
}

impl TransactionExt for Transaction {
    fn verify_scripts(
        &self,
        view: &CoinView,
        flags: &ScriptFlags,
    ) -> Result<(), TransactionVerificationError> {
        let mut spending_transaction = Vec::with_capacity(self.get_size());
        self.consensus_encode(&mut spending_transaction).unwrap();
        self.input
            .par_iter()
            .enumerate()
            .map(|(input_index, input)| {
                let utxo = &view.map[&input.previous_output];
                let amount = utxo.output.value;
                let spent_output = utxo.output.script_pubkey.as_bytes();
                bitcoinconsensus::verify_with_flags(
                    spent_output,
                    amount,
                    &spending_transaction,
                    input_index,
                    flags.bits(),
                )
                .map_err(|_| TransactionVerificationError::InvalidScripts)
            })
            .collect()
    }

    // bcoin spends the inputs before check inputs so coin.spent is true
    // whereas bitcoin core does so afterwards
    fn check_inputs(
        &self,
        view: &CoinView,
        height: u32,
    ) -> Result<u64, TransactionVerificationError> {
        use TransactionVerificationError::*;

        let mut total: u64 = 0;

        for input in &self.input {
            let coin = match view.get_entry(&input.previous_output) {
                None => return Err(InputsMissingOrSpent),
                Some(coin) => coin,
            };

            // ensure!(!coin.spent);

            if coin.coinbase {
                let coin_height = coin
                    .height
                    .expect("immature coinbase spend found in mempool");
                if height.saturating_sub(coin_height) < consensus::COINBASE_MATURITY {
                    return Err(PrematureCoinbaseSpend);
                }
            }

            total = total
                .checked_add(coin.output.value)
                .ok_or(InputValuesOutOfRange)?;

            if coin.output.value > consensus::MAX_MONEY || total > consensus::MAX_MONEY {
                return Err(InputValuesOutOfRange);
            }
        }

        let value = self.get_output_value();

        if total < value {
            return Err(InBelowOut);
        }

        let fee = total - value;

        if fee > consensus::MAX_MONEY {
            return Err(FeeOutOfRange);
        }

        Ok(fee)
    }

    // Does not check overflows, must be done separately
    fn get_output_value(&self) -> u64 {
        self.output
            .iter()
            .fold(0, |total, output| total + output.value)
    }

    // TODO: use option rather than i32
    fn is_final(&self, height: u32, time: i32) -> bool {
        if self.lock_time == 0 {
            return true;
        }

        let lock_time = if self.lock_time < consensus::LOCKTIME_THRESHOLD {
            height as i32
        } else {
            time
        };

        if (self.lock_time as i32) < lock_time {
            return true;
        }

        for input in &self.input {
            if input.sequence != 0xffffffff {
                return false;
            }
        }

        true
    }

    fn get_sigop_cost(&self, view: &CoinView, flags: ScriptFlags) -> usize {
        let mut sig_ops = self.get_legacy_sig_op_count() * WITNESS_SCALE_FACTOR;

        if self.is_coin_base() {
            return sig_ops;
        }

        if flags.contains(ScriptFlags::VERIFY_P2SH) {
            sig_ops += self.get_p2sh_sig_op_count(view) * WITNESS_SCALE_FACTOR;
        }

        if flags.contains(ScriptFlags::VERIFY_WITNESS) {
            sig_ops += self.get_witness_sig_op_count(view);
        }

        sig_ops
    }

    /// Get the legacy signature operation count which is quite inaccurate
    fn get_legacy_sig_op_count(&self) -> usize {
        let inputs_count: usize = self
            .input
            .iter()
            .map(|input| input.script_sig.get_sig_op_count(false))
            .sum();

        let outputs_count: usize = self
            .output
            .iter()
            .map(|output| output.script_pubkey.get_sig_op_count(false))
            .sum();

        inputs_count + outputs_count
    }

    fn get_p2sh_sig_op_count(&self, view: &CoinView) -> usize {
        if self.is_coin_base() {
            return 0;
        }

        let mut sig_ops = 0;

        for input in &self.input {
            if let Some(previous_output) = view.get_output(&input.previous_output) {
                if previous_output.script_pubkey.is_p2sh() {
                    sig_ops += previous_output
                        .script_pubkey
                        .get_p2sh_sig_op_count(&input.script_sig);
                }
            }
        }

        sig_ops
    }

    fn get_witness_sig_op_count(&self, view: &CoinView) -> usize {
        let mut sig_ops = 0;

        for input in &self.input {
            if let Some(previous_output) = view.get_output(&input.previous_output) {
                sig_ops += previous_output
                    .script_pubkey
                    .get_witness_sig_op_count(&input.script_sig, &input.witness);
            }
        }

        sig_ops
    }

    fn check_sanity(&self) -> Result<(), TransactionVerificationError> {
        use TransactionVerificationError::*;

        if self.input.is_empty() {
            return Err(InputsEmpty);
        }

        if self.output.is_empty() {
            return Err(OutputsEmpty);
        }

        if self.stripped_size() * WITNESS_SCALE_FACTOR > MAX_BLOCK_WEIGHT as usize {
            return Err(Oversized);
        }

        let mut value_out: u64 = 0;
        for output in &self.output {
            if output.value > MAX_MONEY {
                return Err(OutputTooLarge);
            }
            value_out = value_out
                .checked_add(output.value)
                .ok_or_else(|| OutputTotalTooLarge)?;
            if value_out > MAX_MONEY {
                return Err(OutputTotalTooLarge);
            }
        }

        // can only be duplicate inputs if more than 1
        if self.input.len() > 1 {
            // Check for duplicate inputs
            let mut outpoints = HashSet::with_capacity(self.input.len());
            for input in &self.input {
                let duplicate = !outpoints.insert(&input.previous_output);
                if duplicate {
                    return Err(DuplicateInput);
                }
            }
        }

        if self.is_coin_base() {
            let script_sig_len = self.input[0].script_sig.len();
            if script_sig_len < 2 || script_sig_len > 100 {
                return Err(BadCoinbaseLength);
            }
        } else {
            for input in &self.input {
                if input.previous_output.is_null() {
                    return Err(NullPreviousOutput);
                }
            }
        }

        Ok(())
    }

    /// Test whether the transaction has a non-empty witness.
    fn has_witness(&self) -> bool {
        self.input.iter().any(|input| !input.witness.is_empty())
    }

    /// Transaction size without witness
    fn stripped_size(&self) -> usize {
        let mut input_weight = 0;
        for input in &self.input {
            input_weight += 32 + 4 + 4 + // outpoint (32+4) + nSequence
                VarInt(input.script_sig.len() as u64).len() +
                input.script_sig.len();
        }
        let mut output_size = 0;
        for output in &self.output {
            output_size += 8 + // value
                VarInt(output.script_pubkey.len() as u64).len() +
                output.script_pubkey.len();
        }
        let non_input_size =
        // version:
        4 +
        // count varints:
        VarInt(self.input.len() as u64).len() +
        VarInt(self.output.len() as u64).len() +
        output_size +
        // lock_time
        4;

        non_input_size + input_weight
    }

    /// Signals RBF
    fn signals_replacement(&self) -> bool {
        self.input
            .iter()
            .any(|input| input.sequence < u32::max_value() - 1)
    }
}
