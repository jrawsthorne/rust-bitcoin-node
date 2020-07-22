use crate::protocol::consensus::{
    MAX_PUBKEYS_PER_MULTISIG, WITNESS_V0_KEYHASH_SIZE, WITNESS_V0_SCRIPTHASH_SIZE,
};
use bitcoin::{
    blockdata::{opcodes, script::Instruction},
    Script,
};

pub trait ScriptExt {
    fn is_multisig(&self) -> bool;
    fn get_sig_op_count(&self, accurate: bool) -> usize;
    fn get_redeem_script(&self) -> Option<Script>;
    fn get_witness_sig_op_count(&self, script_sig: &Script, witness: &[Vec<u8>]) -> usize;
    fn get_p2sh_sig_op_count(&self, script_sig: &Script) -> usize;
    fn get_witness_program(&self) -> Option<(usize, &[u8])>;
}

impl ScriptExt for Script {
    fn is_multisig(&self) -> bool {
        use bitcoin::blockdata::opcodes::all::{OP_CHECKMULTISIG, OP_PUSHNUM_1, OP_PUSHNUM_16};
        // multisig is m <n pubkeys> n OP_CHECKMULTISIG

        fn read_small_int(byte: u8) -> Option<u8> {
            if byte >= OP_PUSHNUM_1.into_u8() && byte <= OP_PUSHNUM_16.into_u8() {
                Some(byte - 0x50)
            } else {
                None
            }
        }

        let bytes = &self[..];
        if self.len() < 3 || bytes[bytes.len() - 1] != OP_CHECKMULTISIG.into_u8() {
            return false;
        }
        let m = match read_small_int(bytes[0]) {
            None => return false,
            Some(m) => m,
        };
        let n = match read_small_int(bytes[bytes.len() - 2]) {
            None => return false,
            Some(n) => n,
        };
        if m > n {
            return false;
        }
        let mut keys = 0;

        for instruction in self.iter(false).skip(1).take(n as usize) {
            match instruction {
                Instruction::PushBytes(bytes) if bytes.len() == 33 || bytes.len() == 65 => {
                    keys += 1;
                }
                _ => return false,
            }
        }

        keys == n
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{
        blockdata::script::Builder, consensus::deserialize, hashes::hex::FromHex, PublicKey,
        Transaction,
    };

    #[test]
    fn test_multisig() {
        let tx:Transaction=deserialize(&Vec::from_hex("010000000337bd40a022eea1edd40a678cddabe200b131afd5797b232ac21861d8e97eb367020000008a4730440220e8343f8ac7e96582d92a450ce314668db4f7a0e2c94a97aa6df026f93ebee2290220866b5728d4247688d91b4a30144762bc8bfd7f385de7f7d326d665ff5e3e900301410461cbdcc5409fb4b4d42b51d33381354d80e550078cb532a34bfa2fcfdeb7d76519aecc62770f5b0e4ef8551946d8a540911abe3e7854a26f39f58b25c15342afffffffff96420befb14a9357181e5da089824a3e6ea5a95856ff74c06c7d5ea98d633cf9020000008a4730440220b7227a8f816f3810f97057102edf8be4434c1e00f48b4440976bcc478f1431030220af3cba150afdd44618de4369cdc65fea73e447d7b5fbe135d2f08f86d82aa85f01410461cbdcc5409fb4b4d42b51d33381354d80e550078cb532a34bfa2fcfdeb7d76519aecc62770f5b0e4ef8551946d8a540911abe3e7854a26f39f58b25c15342afffffffff96420befb14a9357181e5da089824a3e6ea5a95856ff74c06c7d5ea98d633cf9010000008a47304402207d689e1a61e06440eab18d961517a97c49219a91f2c59d9630e902fcb2f4ea8b0220dcd274349ca264d8bd2bee5135664a92899e94a319a349d6d6e3660d04b564ad0141047a4c5d104002ebc203bef5cab6f13ff57ab624bb5f9f1186beb64c83a396da0d912e11a18ea15a2c784a62fed2bbd8258c3413c18bf4c3f2ba28f3d5565e328bffffffff0340420f000000000087514104cc71eb30d653c0c3163990c47b976f3fb3f37cccdcbedb169a1dfef58bbfbfaff7d8a473e7e2e6d317b87bafe8bde97e3cf8f065dec022b51d11fcdd0d348ac4410461cbdcc5409fb4b4d42b51d33381354d80e550078cb532a34bfa2fcfdeb7d76519aecc62770f5b0e4ef8551946d8a540911abe3e7854a26f39f58b25c15342af52ae50cec402000000001976a914c812a297b8e0e778d7a22bb2cd6d23c3e789472b88ac20a10700000000001976a914641ad5051edd97029a003fe9efb29359fcee409d88ac00000000").unwrap()).unwrap();
        assert!(tx.output[0].script_pubkey.is_multisig());

        let random = Builder::new()
            .push_int(1)
            .push_key(&PublicKey::from_slice(&Vec::from_hex("0447d5317e5c2c9fded801aadc8daed23294f0742b14bc681ef45c0a18115a8674e92a8f73c2d33702690a620eb1edf78b4ddcdc941a7405d762cd5203289a2f93").unwrap()).unwrap())
            .push_int(1)
            .push_opcode(bitcoin::blockdata::opcodes::all::OP_CHECKMULTISIG)
            .into_script();

        assert!(random.is_multisig());
    }
}
