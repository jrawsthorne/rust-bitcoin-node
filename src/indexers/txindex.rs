use crate::{
    blockchain::{Chain, ChainListener},
    db::{Batch, DBKey, Database, DiskDatabase},
    ChainEntry,
};
use bitcoin::{
    consensus::{deserialize, Decodable, Encodable},
    Block, Transaction, Txid, VarInt,
};
use parking_lot::RwLock;
use std::{path::Path, sync::Arc};

pub struct TxIndexer {
    db: DiskDatabase,
}

impl ChainListener for Arc<RwLock<TxIndexer>> {
    fn handle_connect(
        &self,
        _chain: &Chain,
        entry: &ChainEntry,
        block: &Block,
        _view: &crate::CoinView,
    ) {
        self.write().index_block(entry, block);
    }

    fn handle_disconnect(
        &self,
        _chain: &Chain,
        _entry: &ChainEntry,
        block: &Block,
        _view: &crate::CoinView,
    ) {
        self.write().unindex_block(block);
    }
}

pub struct TxMap(u32, u32, u32);

impl Encodable for TxMap {
    fn consensus_encode<W: std::io::Write>(
        &self,
        mut e: W,
    ) -> Result<usize, bitcoin::consensus::encode::Error> {
        Ok(self.0.consensus_encode(&mut e)?
            + self.1.consensus_encode(&mut e)?
            + self.2.consensus_encode(&mut e)?)
    }
}

impl Decodable for TxMap {
    fn consensus_decode<D: std::io::Read>(
        mut d: D,
    ) -> Result<Self, bitcoin::consensus::encode::Error> {
        let height = u32::consensus_decode(&mut d)?;
        let offset = u32::consensus_decode(&mut d)?;
        let length = u32::consensus_decode(&mut d)?;
        Ok(TxMap(height, offset, length))
    }
}

pub const COL_TRANSACTION: &str = "T";

impl DBKey for Txid {
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
            batch.insert(
                tx.txid(),
                &TxMap(entry.height, offset as u32, length as u32),
            );
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
