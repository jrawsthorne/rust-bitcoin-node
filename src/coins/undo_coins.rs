use std::io;

use crate::{CoinEntry, CoinView};
use bitcoin::{
    consensus::{encode, Decodable, Encodable},
    OutPoint,
};

#[derive(Default, Debug, Clone)]
pub struct UndoCoins {
    pub(crate) items: Vec<CoinEntry>,
}

impl UndoCoins {
    pub fn push(&mut self, coin: CoinEntry) {
        self.items.push(coin);
    }

    pub fn apply(&mut self, view: &mut CoinView, prevout: OutPoint) {
        let undo = self.items.pop().expect("bad code");

        view.map.insert(prevout, undo);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl Encodable for UndoCoins {
    fn consensus_encode<W: std::io::Write>(&self, mut e: W) -> Result<usize, io::Error> {
        let mut len = 0;
        len += (self.items.len() as u32).consensus_encode(&mut e)?;
        for coin in &self.items {
            len += coin.consensus_encode(&mut e)?;
        }
        Ok(len)
    }
}

impl Decodable for UndoCoins {
    fn consensus_decode<D: std::io::Read>(mut d: D) -> Result<Self, encode::Error> {
        let count = u32::consensus_decode(&mut d)?;

        let mut items = Vec::with_capacity(count as usize * std::mem::size_of::<CoinEntry>());

        for _ in 0..count {
            items.push(CoinEntry::consensus_decode(&mut d)?);
        }

        Ok(UndoCoins { items })
    }
}
