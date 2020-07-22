use crate::{
    db::{Batch, DBKey, Database, DiskDatabase, IterDirection},
    ChainEntry, CoinView,
};
use bitcoin::{
    consensus::{encode, Decodable, Encodable},
    hashes::Hash,
    util::address::Payload,
    Address, AddressType, Block, Network, Txid, VarInt,
};
use std::{collections::HashSet, convert::TryInto, io::Cursor, path::Path};

pub struct AddrIndexer {
    db: DiskDatabase<Key>,
    network: Network,
}

pub enum Key {
    AddrMap(Address, u32, u32),
    Address(Address),
    TxidByHeightAndIndex(u32, u32),
}

impl DBKey for Key {
    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        let mut encoder = vec![];
        let addr = match self {
            Key::TxidByHeightAndIndex(height, index) => {
                height.consensus_encode(&mut encoder)?;
                index.consensus_encode(&mut encoder)?;
                return Ok(encoder);
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
        address_type.consensus_encode(&mut encoder)?;
        match &addr.payload {
            Payload::PubkeyHash(hash) => {
                hash.into_inner().to_vec().consensus_encode(&mut encoder)?;
            }
            Payload::ScriptHash(hash) => {
                hash.into_inner().to_vec().consensus_encode(&mut encoder)?;
            }
            Payload::WitnessProgram { program, .. } => {
                program.consensus_encode(&mut encoder)?;
            }
        }
        if let Key::AddrMap(_, height, index) = self {
            height.to_be_bytes().consensus_encode(&mut encoder)?;
            index.to_be_bytes().consensus_encode(&mut encoder)?;
        }
        Ok(encoder)
    }

    fn col(&self) -> &'static str {
        COL_ADDR
    }
}

pub const COL_ADDR: &str = "A";

impl DBKey for Address {
    fn encode(&self) -> Result<Vec<u8>, encode::Error> {
        let mut encoder = vec![];
        let address_type: u8 = match self.address_type().expect("bad address type") {
            AddressType::P2pkh => 0,
            AddressType::P2sh => 1,
            AddressType::P2wpkh => 2,
            AddressType::P2wsh => 3,
        };
        address_type.consensus_encode(&mut encoder)?;
        match &self.payload {
            Payload::PubkeyHash(hash) => {
                hash.into_inner().to_vec().consensus_encode(&mut encoder)?;
            }
            Payload::ScriptHash(hash) => {
                hash.into_inner().to_vec().consensus_encode(&mut encoder)?;
            }
            Payload::WitnessProgram { program, .. } => {
                program.consensus_encode(&mut encoder)?;
            }
        }
        Ok(encoder)
    }

    fn col(&self) -> &'static str {
        COL_ADDR
    }
}

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
                            batch
                                .insert(Key::AddrMap(addr, entry.height, index as u32), DUMMY)
                                .unwrap();
                        }
                    }
                }
            }
            for output in &tx.output {
                if let Some(addr) = Address::from_script(&output.script_pubkey, self.network) {
                    if addr.is_standard() {
                        has_address = true;
                        batch
                            .insert(Key::AddrMap(addr, entry.height, index as u32), DUMMY)
                            .unwrap();
                    }
                }
            }
            if has_address {
                batch
                    .insert(
                        Key::TxidByHeightAndIndex(entry.height, index as u32),
                        &tx.txid(),
                    )
                    .unwrap();
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

        let max = Key::AddrMap(address.clone(), u32::max_value(), u32::max_value())
            .encode()
            .unwrap()
            .into_boxed_slice();

        for (key, _) in self
            .db
            .iter_cf::<Vec<u8>>(
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
