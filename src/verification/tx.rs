use crate::protocol::consensus::{self, ScriptFlags};
use crate::{error::TransactionVerificationError, CoinView};
use bitcoin::{
    blockdata::{constants::*, opcodes, script::Instruction},
    consensus::encode::serialize,
    Script, Transaction, VarInt,
};
use consensus::*;
use rayon::prelude::*;
use std::collections::HashSet;

pub trait TransactionVerifier {
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
}

impl TransactionVerifier for Transaction {
    fn verify_scripts(
        &self,
        view: &CoinView,
        flags: &ScriptFlags,
    ) -> Result<(), TransactionVerificationError> {
        let spending_transaction = serialize(self);
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

            // TODO: height might be none if mempool child transaction, don't unwrap
            if coin.coinbase && height - coin.height.unwrap() < consensus::COINBASE_MATURITY {
                return Err(PrematureCoinbaseSpend);
            }

            total = total
                .checked_add(coin.output.value)
                .ok_or_else(|| InputValuesOutOfRange)?;

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

        if tx_stripped_size(self) * WITNESS_SCALE_FACTOR > MAX_BLOCK_WEIGHT as usize {
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
}

trait SigOps {
    fn get_sig_op_count(&self, accurate: bool) -> usize;
    fn get_redeem_script(&self) -> Option<Script>;
    fn get_witness_sig_op_count(&self, script_sig: &Script, witness: &[Vec<u8>]) -> usize;
    fn get_p2sh_sig_op_count(&self, script_sig: &Script) -> usize;
    fn get_witness_program(&self) -> Option<(usize, &[u8])>;
}

impl SigOps for Script {
    /// Gets the witness program if this script pubkey is one
    fn get_witness_program(&self) -> Option<(usize, &[u8])> {
        if !self.is_witness_program() {
            return None;
        }
        let version = self.as_bytes()[0] as usize;
        let program = &self.as_bytes()[2..];
        Some((version, program))
    }

    /// The number of signature operations in a script
    /// If accurate is false, multisig operations use up the maximum of 20 pubkeys
    fn get_sig_op_count(&self, accurate: bool) -> usize {
        use bitcoin::blockdata::opcodes::all::*;

        let mut n = 0;
        let mut last_opcode = OP_RETURN_255.into_u8();

        for op in self.iter(false) {
            match op {
                Instruction::Error(_) => break,
                Instruction::Op(op) => {
                    match op {
                        OP_CHECKSIG | OP_CHECKSIGVERIFY => {
                            n += 1;
                        }
                        OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => {
                            if accurate
                                && last_opcode >= OP_PUSHNUM_1.into_u8()
                                && last_opcode <= OP_PUSHNUM_16.into_u8()
                            {
                                n += last_opcode as usize - 0x50;
                            } else {
                                n += MAX_PUBKEYS_PER_MULTISIG;
                            }
                        }
                        _ => {}
                    }
                    last_opcode = op.into_u8();
                }
                Instruction::PushBytes(_) => {}
            }
        }

        n
    }

    /// The number of signature operations for a p2sh redeem script
    /// Note that the accurate version of `get_sig_op_count` is used
    fn get_p2sh_sig_op_count(&self, script_sig: &Script) -> usize {
        if !self.is_p2sh() {
            return self.get_sig_op_count(true);
        }

        if let Some(redeem_script) = script_sig.get_redeem_script() {
            redeem_script.get_sig_op_count(true)
        } else {
            0
        }
    }

    /// If this script spends a p2sh output, get the redeem script from the script_sig
    fn get_redeem_script(&self) -> Option<Script> {
        // the p2sh redeem script is the last item that the script_sig
        // pushes onto the stack

        let mut data = None;

        for op in self.iter(false) {
            match op {
                Instruction::Error(_) => return None,
                Instruction::Op(op) => {
                    if op.into_u8() > opcodes::all::OP_PUSHNUM_16.into_u8() {
                        return None;
                    }
                }
                Instruction::PushBytes(bytes) => data = Some(bytes),
            }
        }

        Some(Script::from(data?.to_vec()))
    }

    fn get_witness_sig_op_count(&self, script_sig: &Script, witness: &[Vec<u8>]) -> usize {
        if let Some((version, program)) = self.get_witness_program() {
            return witness_sig_op_count(version, program, witness);
        } else if self.is_p2sh() {
            if let Some(redeem_script) = script_sig.get_redeem_script() {
                if let Some((version, program)) = redeem_script.get_witness_program() {
                    return witness_sig_op_count(version, program, witness);
                }
            }
        }
        0
    }
}

/// Signature operations for segwit
fn witness_sig_op_count(version: usize, program: &[u8], witness: &[Vec<u8>]) -> usize {
    if version == 0 {
        if program.len() == WITNESS_V0_KEYHASH_SIZE {
            return 1;
        }

        if program.len() == WITNESS_V0_SCRIPTHASH_SIZE && !witness.is_empty() {
            let redeem_script = witness.last().unwrap();
            return Script::from(redeem_script.to_vec()).get_sig_op_count(true);
        }
    }
    // Future flags may be implemented here.
    // TODO: Any changes for taproot?
    0
}

/// Transaction size without witness
pub fn tx_stripped_size(tx: &Transaction) -> usize {
    let mut input_weight = 0;
    for input in &tx.input {
        input_weight += 32 + 4 + 4 + // outpoint (32+4) + nSequence
                VarInt(input.script_sig.len() as u64).len() +
                input.script_sig.len();
    }
    let mut output_size = 0;
    for output in &tx.output {
        output_size += 8 + // value
                VarInt(output.script_pubkey.len() as u64).len() +
                output.script_pubkey.len();
    }
    let non_input_size =
        // version:
        4 +
        // count varints:
        VarInt(tx.input.len() as u64).len() +
        VarInt(tx.output.len() as u64).len() +
        output_size +
        // lock_time
        4;

    non_input_size + input_weight
}
