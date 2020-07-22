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
    fn encode(&self) -> Result<Vec<u8>, bitcoin::consensus::encode::Error> {
        Ok(match self {
            Key::BlockRecord(record_type, hash) => {
                let mut encoder = Vec::with_capacity(36);
                record_type.as_u32().consensus_encode(&mut encoder)?;
                hash.consensus_encode(&mut encoder)?;
                encoder
            }
            Key::LastFile(record_type) => record_type.as_u32().to_le_bytes().to_vec(),
            Key::File(record_type, fileno) => {
                let mut encoder = Vec::with_capacity(36);
                record_type.as_u32().consensus_encode(&mut encoder)?;
                fileno.consensus_encode(&mut encoder)?;
                encoder
            }
        })
    }

    fn col(&self) -> &'static str {
        match self {
            Key::LastFile(_) => COL_LAST_FILE,
            Key::BlockRecord(_, _) => COL_BLOCK_RECORD,
            Key::File(_, _) => COL_FILE,
        }
    }
}
