use crate::protocol::consensus;
use crate::util::EmptyResult;
use crate::verification::{CheckQueueControl, ScriptVerification};
use crate::CoinView;
use bitcoin::{
    blockdata::{opcodes, script::Instruction},
    consensus::encode::serialize,
    util::amount::Amount,
    Script, Transaction,
};
use consensus::*;
use failure::{bail, ensure, err_msg, Error};
use std::{collections::HashSet, sync::Arc};

pub trait TransactionVerifier {
    fn check_inputs(&self, view: &CoinView, height: u32) -> Result<Amount, Error>;
    fn get_output_value(&self) -> u64;
    fn is_final(&self, height: u32, time: u32) -> bool;
    fn get_sigop_cost(&self, view: &CoinView) -> usize;
    fn get_legacy_sig_op_count(&self) -> usize;
    fn get_p2sh_sig_op_count(&self, view: &CoinView) -> usize;
    fn get_witness_sig_op_count(&self, view: &CoinView) -> usize;
    fn push_verification(&self, view: &CoinView, flags: u32, verifier: &CheckQueueControl);
    fn check_sanity(&self) -> EmptyResult;
}

impl TransactionVerifier for Transaction {
    fn push_verification(&self, view: &CoinView, flags: u32, verifier: &CheckQueueControl) {
        let raw = Arc::new(serialize(self));

        let mut checks = vec![];

        for (index, input) in self.input.iter().enumerate() {
            let previous_output = view
                .get_output(&input.previous_output)
                .expect("Output not found")
                .clone();
            let check = ScriptVerification {
                raw_tx: Arc::clone(&raw),
                index,
                previous_output,
                flags,
            };
            checks.push(check);
        }

        verifier.add(checks);
    }

    // bcoin spends the inputs before check inputs so coin.spent is true
    // whereas bitcoin core does so afterwards
    fn check_inputs(&self, view: &CoinView, height: u32) -> Result<Amount, Error> {
        let mut total = Amount::ZERO;

        for input in &self.input {
            let coin = match view.get_entry(&input.previous_output) {
                Some(coin) => coin,
                None => bail!("bad-txns-inputs-missingorspent"),
            };

            // ensure!(!coin.spent);

            if coin.coinbase {
                ensure!(height - coin.height.unwrap() >= consensus::COINBASE_MATURITY);
            }

            total = total
                .checked_add(Amount::from_sat(coin.output.value))
                .ok_or(err_msg("bad-txns-inputvalues-outofrange"))?;

            if coin.output.value > consensus::MAX_MONEY || total.as_sat() > consensus::MAX_MONEY {
                bail!("bad-txns-inputvalues-outofrange");
            }
        }

        let value = Amount::from_sat(self.get_output_value());

        if total < value {
            bail!("bad-txns-in-belowout");
        }

        let fee = total - value;

        if fee.as_sat() > consensus::MAX_MONEY {
            bail!("bad-txns-fee-outofrange");
        }

        Ok(total)
    }

    // Does not check overflows, must be done separately
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

    fn get_sigop_cost(&self, view: &CoinView) -> usize {
        let mut sig_ops = self.get_legacy_sig_op_count() * WITNESS_SCALE_FACTOR;

        if self.is_coin_base() {
            return sig_ops;
        }

        sig_ops += self.get_p2sh_sig_op_count(view) * WITNESS_SCALE_FACTOR;

        sig_ops += self.get_witness_sig_op_count(view);

        sig_ops
    }

    /// Get the legacy signature operation count which is quite inaccurate
    fn get_legacy_sig_op_count(&self) -> usize {
        let mut sig_ops = 0;
        for input in &self.input {
            sig_ops += input.script_sig.get_sig_op_count(false);
        }
        for output in &self.output {
            sig_ops += output.script_pubkey.get_sig_op_count(false);
        }
        sig_ops
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

    fn check_sanity(&self) -> EmptyResult {
        ensure!(!self.input.is_empty(), "bad-txns-vin-empty");
        ensure!(!self.output.is_empty(), "bad-txns-vout-empty");

        // TODO: Serialize size without witness as that hasn't been checked for malleability

        let mut value_out = Amount::ZERO;
        for output in &self.output {
            ensure!(output.value <= MAX_MONEY, "bad-txns-vout-toolarge");
            value_out = value_out
                .checked_add(Amount::from_sat(output.value))
                .ok_or(err_msg("bad-txns-txouttotal-toolarge"))?;
            ensure!(
                value_out.as_sat() <= MAX_MONEY,
                "bad-txns-txouttotal-toolarge"
            );
        }

        // Check for duplicate inputs
        let mut outpoints = HashSet::with_capacity(self.input.len());
        for input in &self.input {
            ensure!(
                outpoints.insert(input.previous_output),
                "bad-txns-inputs-duplicate"
            );
        }

        if self.is_coin_base() {
            let script_sig_len = self.input[0].script_sig.len();
            ensure!(
                script_sig_len >= 2 && script_sig_len <= 100,
                "bad-cb-length"
            );
        } else {
            for input in &self.input {
                ensure!(!input.previous_output.is_null(), "bad-txns-prevout-null");
            }
        }

        Ok(())
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

        if program.len() == WITNESS_V0_SCRIPTHASH_SIZE && witness.len() > 0 {
            let redeem_script = witness.last().unwrap();
            return Script::from(redeem_script.to_vec()).get_sig_op_count(true);
        }
    }
    // Future flags may be implemented here.
    0
}
