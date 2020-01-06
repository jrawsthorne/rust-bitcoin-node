use rust_bitcoin_node::protocol::{NetworkParams, BASE_REWARD};
use rust_bitcoin_node::{
    blockchain::{Chain, ChainOptions},
    mining::Miner,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn chain(path: PathBuf) -> Chain {
    let network_params = NetworkParams::from_network(bitcoin::Network::Regtest);
    let mut chain = Chain::new(ChainOptions {
        network: network_params,
        path,
    });
    chain.open().unwrap();
    chain
}

#[test]
fn mine_200_blocks() {
    let tmp_dir = TempDir::new().unwrap();
    let mut chain = chain(tmp_dir.path().into());
    let miner = Miner::new();

    for _ in 0..200 {
        let tip = chain.tip.clone();
        let block_template = miner.create_block(tip, None);
        let block = Miner::mine_block(block_template);
        assert!(chain.add(&block).unwrap().is_some());
    }

    assert_eq!(chain.height, 200);
    assert_eq!(chain.db.state.value, 200 * BASE_REWARD);
    assert_eq!(chain.db.state.coin, 200);
    assert_eq!(chain.db.state.tx, 201);
}

// #[test]
// fn should_mine_competing_chains() {
//     let tmp_dir = TempDir::new().unwrap();

//     let mut chain = chain(tmp_dir.path().into());

//     let miner = Miner::new();

//     let tip1 = chain.tip.clone();
//     let tip2 = chain.tip.clone();

//     let block1 = miner.create_block(tip1, None);
//     let block2 = miner.create_block(tip2, None);

//     let block1 = Miner::mine_block(block1);
//     let block2 = Miner::mine_block(block2);

//     chain.add(&block1).unwrap();
//     chain.add(&block2).unwrap();
// }
