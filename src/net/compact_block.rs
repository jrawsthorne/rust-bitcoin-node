use bitcoin::{
    hashes::sha256d,
    util::bip152::{BlockTransactionsRequest, HeaderAndShortIds, PrefilledTransaction, ShortId},
    Block, BlockHeader, Transaction,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct CompactBlock {
    pub available: Vec<Option<Transaction>>,
    pub header: BlockHeader,
    pub sip_key: (u64, u64),
    pub total_tx: usize,
    pub count: usize,
    pub id_map: HashMap<ShortId, usize>,
}

impl CompactBlock {
    pub fn new(header_and_short_ids: HeaderAndShortIds) -> Option<Self> {
        let HeaderAndShortIds {
            header,
            nonce,
            prefilled_txs,
            short_ids,
        } = header_and_short_ids;

        let total_tx = prefilled_txs.len() + short_ids.len();

        let mut available = vec![None; total_tx];

        let mut last = -1;
        let mut count = 0;

        for PrefilledTransaction(index, tx) in prefilled_txs {
            last += index as i64 + 1;
            assert!(last < 0xffff);
            assert!(last <= short_ids.len() as i64 + 1);
            available[last as usize] = Some(tx);
            count += 1;
        }

        let mut offset = 0;
        let mut id_map = HashMap::with_capacity(short_ids.len());

        for (i, id) in short_ids.into_iter().enumerate() {
            while available[i + offset].is_some() {
                offset += 1;
            }

            if id_map.contains_key(&id) {
                return None;
            }

            id_map.insert(id, i + offset);
        }

        Some(Self {
            total_tx,
            header,
            sip_key: ShortId::calculate_siphash_keys(&header, nonce),
            available,
            count,
            id_map,
        })
    }

    pub fn fill_mempool<'a>(&mut self, mempool: impl Iterator<Item = &'a Transaction>) -> bool {
        if self.count == self.total_tx {
            return true;
        }

        let mut set = HashSet::new();

        for tx in mempool {
            let txid = tx.wtxid().as_hash();

            let short_id = self.short_id(&txid);

            let index = match self.id_map.get(&short_id) {
                None => continue,
                Some(&index) => index,
            };
            if set.contains(&index) {
                // Siphash collision, just request it.
                self.available[index] = None;
                self.count -= 1;
                continue;
            }
            self.available[index] = Some(tx.clone());
            set.insert(index);
            self.count += 1;
            if self.count == self.total_tx {
                return true;
            }
        }

        false
    }

    pub fn fill_missing(&mut self, transactions: Vec<Transaction>) -> bool {
        let mut transactions = transactions.into_iter();
        let mut missing = self.available.iter_mut().filter(|a| a.is_none());

        loop {
            match (transactions.next(), missing.next()) {
                // missing as well as transaction to replace it
                (Some(tx), Some(missing)) => {
                    missing.replace(tx);
                }
                // no more missing and no more transactions
                (None, None) => break true,
                // either not enough transactions to fill missing or more transactions than missing
                _ => break false,
            }
        }
    }

    fn short_id(&self, txid: &sha256d::Hash) -> ShortId {
        ShortId::with_siphash_keys(txid, self.sip_key)
    }

    pub fn into_block_transactions_request(&self) -> BlockTransactionsRequest {
        let indexes = self
            .available
            .iter()
            .enumerate()
            .filter_map(|(index, tx)| {
                if tx.is_none() {
                    Some(index as u64)
                } else {
                    None
                }
            })
            .collect();
        BlockTransactionsRequest {
            block_hash: self.header.block_hash().as_hash(),
            indexes,
        }
    }
}

impl From<CompactBlock> for Block {
    fn from(block: CompactBlock) -> Self {
        Self {
            header: block.header,
            txdata: block.available.into_iter().map(Option::unwrap).collect(),
        }
    }
}
