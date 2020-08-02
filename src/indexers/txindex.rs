use crate::{
    blockchain::Chain,
    db::{Batch, DBKey, DBValue, Database, DiskDatabase},
    ChainEntry,
};
use bitcoin::{
    consensus::{deserialize, Decodable, Encodable},
    Block, Transaction, Txid, VarInt,
};
use std::path::Path;

pub struct TxIndexer {
    db: DiskDatabase<Txid>,
}

pub struct TxMap(u32, u32, u32);

impl DBValue for TxMap {
    fn decode<R: std::io::Read>(mut decoder: R) -> Result<Self, bitcoin::consensus::encode::Error> {
        let height = u32::consensus_decode(&mut decoder)?;
        let offset = u32::consensus_decode(&mut decoder)?;
        let length = u32::consensus_decode(&mut decoder)?;

        Ok(Self(height, offset, length))
    }

    fn encode(&self) -> Result<Vec<u8>, bitcoin::consensus::encode::Error> {
        let mut encoder = Vec::with_capacity(12);
        self.0.consensus_encode(&mut encoder)?;
        self.1.consensus_encode(&mut encoder)?;
        self.2.consensus_encode(&mut encoder)?;
        Ok(encoder)
    }
}

pub const COL_TRANSACTION: &str = "T";

impl DBKey for Txid {
    fn encode(&self) -> Result<Vec<u8>, bitcoin::consensus::encode::Error> {
        Ok(self.as_ref().into())
    }

    fn col(&self) -> &'static str {
        COL_TRANSACTION
    }
}

impl TxIndexer {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            db: DiskDatabase::new(path, vec![COL_TRANSACTION]),
        }
    }
    pub fn index_block(&mut self, entry: &ChainEntry, block: &Block) {
        let mut batch = Batch::new();
        let mut offset = 80 + VarInt(block.txdata.len() as u64).len(); // header
        for tx in &block.txdata {
            let length = tx.get_size();
            batch
                .insert(
                    tx.txid(),
                    &TxMap(entry.height, offset as u32, length as u32),
                )
                .unwrap();
            offset += length;
        }
        self.db.write_batch(batch).unwrap();
    }

    pub fn unindex_block(&mut self, block: &Block) {
        let mut batch = Batch::new();
        for tx in &block.txdata {
            batch.remove(tx.txid());
        }
        self.db.write_batch(batch).unwrap();
    }

    pub fn tx(&self, chain: &Chain, txid: Txid) -> Option<Transaction> {
        let TxMap(height, offset, length) = self.db.get(txid).unwrap()?;
        let hash = chain.db.get_entry_by_height(height)?.hash;
        let raw = chain
            .db
            .blocks
            .read(hash, offset, Some(length))
            .unwrap()
            .unwrap();
        Some(deserialize(&raw).unwrap())
    }
}
