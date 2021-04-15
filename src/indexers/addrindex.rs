use crate::{
    blockchain::ChainListener,
    db::{Batch, DBKey, Database, DiskDatabase, IterDirection},
    ChainEntry, CoinView,
};
use bitcoin::{
    consensus::serialize,
    consensus::{encode, Decodable, Encodable, WriteExt},
    util::address::Payload,
    Address, AddressType, Block, Network, Txid, VarInt,
};
use parking_lot::RwLock;
use std::{
    collections::HashSet,
    convert::TryInto,
    io::{self, Cursor},
    path::Path,
    sync::Arc,
};

pub struct AddrIndexer {
    db: DiskDatabase,
    network: Network,
}

impl ChainListener for Arc<RwLock<AddrIndexer>> {
    fn handle_connect(
        &self,
        _chain: &crate::blockchain::Chain,
        entry: &ChainEntry,
        block: &Block,
        view: &CoinView,
    ) {
        self.write().index_block(entry, block, view);
    }

    fn handle_disconnect(
        &self,
        _chain: &crate::blockchain::Chain,
        entry: &ChainEntry,
        block: &Block,
        view: &CoinView,
    ) {
        self.write().unindex_block(entry, block, view);
    }
}

pub enum Key {
    AddrMap(Address, u32, u32),
    Address(Address),
    TxidByHeightAndIndex(u32, u32),
}

impl DBKey for Key {
    fn col(&self) -> &'static str {
        COL_ADDR
    }
}

impl Encodable for Key {
    fn consensus_encode<W: std::io::Write>(&self, mut e: W) -> Result<usize, io::Error> {
        let mut len = 0;
        let addr = match self {
            Key::TxidByHeightAndIndex(height, index) => {
                len += height.consensus_encode(&mut e)?;
                len += index.consensus_encode(&mut e)?;
                return Ok(len);
            }
            Key::AddrMap(addr, _, _) => addr,
            Key::Address(addr) => addr,
        };
        let address_type: u8 = match addr
            .address_type()
            .expect("non standard address, should have checked")
        {
            AddressType::P2pkh => 0,
            AddressType::P2sh => 1,
            AddressType::P2wpkh => 2,
            AddressType::P2wsh => 3,
        };
        len += address_type.consensus_encode(&mut e)?;
        len += match &addr.payload {
            Payload::PubkeyHash(hash) => {
                e.emit_slice(hash)?;
                hash.len()
            }
            Payload::ScriptHash(hash) => {
                e.emit_slice(hash)?;
                hash.len()
            }
            Payload::WitnessProgram { program, .. } => program.consensus_encode(&mut e)?,
        };
        if let Key::AddrMap(_, height, index) = self {
            len += height.to_be_bytes().consensus_encode(&mut e)?;
            len += index.to_be_bytes().consensus_encode(&mut e)?;
        }
        Ok(len)
    }
}

pub const COL_ADDR: &str = "A";

const DUMMY: &Vec<u8> = &vec![];

impl AddrIndexer {
    pub fn new(network: Network, path: impl AsRef<Path>) -> Self {
        Self {
            network,
            db: DiskDatabase::new(path, vec![COL_ADDR]),
        }
    }

    pub fn index_block(&mut self, entry: &ChainEntry, block: &Block, view: &CoinView) {
        let mut batch = Batch::new();
        for (index, tx) in block.txdata.iter().enumerate() {
            let mut has_address = false;
            if !tx.is_coin_base() {
                for input in &tx.input {
                    let previous_output = &view.map[&input.previous_output].output.script_pubkey;
                    if let Some(addr) = Address::from_script(&previous_output, self.network) {
                        if addr.is_standard() {
                            has_address = true;
                            batch.insert(Key::AddrMap(addr, entry.height, index as u32), DUMMY);
                        }
                    }
                }
            }
            for output in &tx.output {
                if let Some(addr) = Address::from_script(&output.script_pubkey, self.network) {
                    if addr.is_standard() {
                        has_address = true;
                        batch.insert(Key::AddrMap(addr, entry.height, index as u32), DUMMY);
                    }
                }
            }
            if has_address {
                batch.insert(
                    Key::TxidByHeightAndIndex(entry.height, index as u32),
                    &tx.txid(),
                );
            }
        }
        self.db.write_batch(batch).unwrap();
    }

    pub fn unindex_block(&mut self, entry: &ChainEntry, block: &Block, view: &CoinView) {
        let mut batch = Batch::new();
        for (index, tx) in block.txdata.iter().enumerate() {
            let mut has_address = false;
            if !tx.is_coin_base() {
                for input in &tx.input {
                    let previous_output = &view.map[&input.previous_output].output.script_pubkey;

                    if let Some(addr) = Address::from_script(&previous_output, self.network) {
                        has_address = true;
                        if addr.is_standard() {
                            batch.remove(Key::AddrMap(addr, entry.height, index as u32));
                        }
                    }
                }
            }
            for output in &tx.output {
                if let Some(addr) = Address::from_script(&output.script_pubkey, self.network) {
                    if addr.is_standard() {
                        has_address = true;
                        batch.remove(Key::AddrMap(addr, entry.height, index as u32));
                    }
                }
            }
            if has_address {
                batch.remove(Key::TxidByHeightAndIndex(entry.height, index as u32));
            }
        }
        self.db.write_batch(batch).unwrap();
    }

    pub fn txids_for_address(&self, address: Address) -> HashSet<Txid> {
        let mut txids = HashSet::new();

        let max = serialize(&Key::AddrMap(
            address.clone(),
            u32::max_value(),
            u32::max_value(),
        ))
        .into_boxed_slice();

        for (key, _) in self
            .db
            .iter_cf::<Key, Vec<u8>>(
                COL_ADDR,
                crate::db::IterMode::From(Key::Address(address), IterDirection::Forward),
            )
            .unwrap()
        {
            if key > max {
                break;
            }

            let mut decoder = Cursor::new(key);
            // advance
            decoder.set_position(decoder.position() + 1);
            let hash_length = VarInt::consensus_decode(&mut decoder).unwrap();
            decoder.set_position(decoder.position() + hash_length.0);

            let pos = decoder.position() as usize;
            let height = u32::from_be_bytes(decoder.get_ref()[pos..pos + 4].try_into().unwrap());
            decoder.set_position(pos as u64 + 4);

            let pos = decoder.position() as usize;
            let index = u32::from_be_bytes(decoder.get_ref()[pos..pos + 4].try_into().unwrap());
            decoder.set_position(pos as u64 + 4);

            if let Some(txid) = self
                .db
                .get(Key::TxidByHeightAndIndex(height, index))
                .unwrap()
            {
                txids.insert(txid);
            }
        }
        txids
    }
}
