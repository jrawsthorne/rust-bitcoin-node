use log::LevelFilter;
use rust_bitcoin_node::protocol::{NetworkParams, BASE_REWARD};
use rust_bitcoin_node::{
    blockchain::{Chain, ChainOptions},
    mining::Miner,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn chain(path: PathBuf) -> Chain {
    let network_params = NetworkParams::from_network(bitcoin::Network::Regtest);
    Chain::new(
        ChainOptions {
            network: network_params,
            path,
        },
        vec![],
    )
    .unwrap()
}

fn init_logger() {
    let _ = env_logger::builder()
        .filter_module("rust_bitcoin_node", LevelFilter::Debug)
        .format_timestamp_millis()
        .is_test(true)
        .try_init();
}

#[test]
fn mine_200_blocks() {
    init_logger();
    let tmp_dir = TempDir::new().unwrap();
    let mut chain = chain(tmp_dir.path().into());
    let miner = Miner::new();

    for _ in 0..200 {
        let tip = chain.tip;
        let block_template = miner.create_block(tip, None, &mut chain);
        let block = Miner::mine_block(block_template);
        assert!(chain.add(block).is_ok());
    }

    assert_eq!(chain.height, 200);
    assert_eq!(chain.db.state.value, 200 * BASE_REWARD);
    assert_eq!(chain.db.state.coin, 200);
    assert_eq!(chain.db.state.tx, 201);
}

#[test]
fn should_mine_competing_chains() {
    init_logger();
    let tmp_dir = TempDir::new().unwrap();

    let mut chain = chain(tmp_dir.path().into());

    let miner = Miner::new();

    let tip1 = chain.tip.clone();
    let tip2 = chain.tip.clone();

    let block1 = miner.create_block(tip1, None, &mut chain);
    let block2 = miner.create_block(tip2, None, &mut chain);

    let block1 = Miner::mine_block(block1);
    let block2 = Miner::mine_block(block2);

    let hash1 = block1.block_hash();
    let hash2 = block2.block_hash();

    chain.add(block1).unwrap();
    chain.add(block2).unwrap();

    assert_eq!(chain.tip.hash, hash1);

    chain.db.get_entry_by_hash(&hash1).unwrap();
    let tip2 = *chain.db.get_entry_by_hash(&hash2).unwrap();

    assert!(!chain.db.is_main_chain(&tip2));

    assert_eq!(chain.height, 1); // genesis + one main chain block
    assert_eq!(chain.db.state.value, 1 * BASE_REWARD); // one spendable coinbase output
    assert_eq!(chain.db.state.coin, 1);
    assert_eq!(chain.db.state.tx, 2);

    let entry = *chain.db.get_entry_by_hash(&tip2.hash).unwrap();
    assert_eq!(chain.height, entry.height);

    // let block = Miner::mine_block(template);
}

// #[tokio::test]
// async fn should_handle_a_reorg() {
//     init_logger();
//     let tmp_dir = TempDir::new().unwrap();
//     let mut chain = chain(tmp_dir.path().into()).await;
//     let miner = Miner::new();

//     let tip1
// }
