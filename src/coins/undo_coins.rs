use crate::{CoinEntry, CoinView};
use bitcoin::OutPoint;

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
