use std::io;

use super::RecordType;
use crate::db::DBKey;
use bitcoin::{consensus::Encodable, BlockHash};

pub const COL_FILE: &str = "F";
pub const COL_LAST_FILE: &str = "L";
pub const COL_BLOCK_RECORD: &str = "B";

pub fn columns() -> Vec<&'static str> {
    vec![COL_FILE, COL_LAST_FILE, COL_BLOCK_RECORD]
}

pub enum Key {
    LastFile(RecordType),
    File(RecordType, u32),
    BlockRecord(RecordType, BlockHash),
}

impl DBKey for Key {
    fn col(&self) -> &'static str {
        match self {
            Key::LastFile(_) => COL_LAST_FILE,
            Key::BlockRecord(_, _) => COL_BLOCK_RECORD,
            Key::File(_, _) => COL_FILE,
        }
    }
}

impl Encodable for Key {
    fn consensus_encode<W: std::io::Write>(
        &self,
        mut e: W,
    ) -> Result<usize, io::Error> {
        Ok(match self {
            Key::BlockRecord(record_type, hash) => {
                record_type.as_u32().consensus_encode(&mut e)? + hash.consensus_encode(&mut e)?
            }
            Key::LastFile(record_type) => record_type
                .as_u32()
                .to_le_bytes()
                .consensus_encode(&mut e)?,
            Key::File(record_type, fileno) => {
                record_type.as_u32().consensus_encode(&mut e)? + fileno.consensus_encode(&mut e)?
            }
        })
    }
}
