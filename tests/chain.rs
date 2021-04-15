use rust_bitcoin_node::protocol::{NetworkParams, BASE_REWARD};
use rust_bitcoin_node::{
    blockchain::{Chain, ChainOptions},
    mining::Miner,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn chain(path: PathBuf) -> Chain {
    let network_params = NetworkParams::from_network(bitcoin::Network::Regtest);
    Chain::new(ChainOptions {
        network: network_params,
        path,
        verify_scripts: true,
    })
    .unwrap()
}

fn init_logger() {
    let _ = env_logger::builder()
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
        let block = Miner::mine_block(block_template, vec![]);

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

    for _ in 0..10 {
        let tip1 = chain.tip.clone();
        let tip2 = chain.tip.clone();

        let block1 = miner.create_block(tip1, None, &mut chain);
        let block2 = miner.create_block(tip2, None, &mut chain);

        let block1 = Miner::mine_block(block1, vec![]);
        let block2 = Miner::mine_block(block2, vec![]);

        let hash1 = block1.block_hash();
        let hash2 = block2.block_hash();

        chain.add(block1).unwrap();
        chain.add(block2).unwrap();

        assert_eq!(chain.tip.hash, hash1);

        assert!(chain.db.get_entry_by_hash(&hash1).is_some());
        let tip2 = *chain.db.get_entry_by_hash(&hash2).unwrap();

        assert!(!chain.db.is_main_chain(&tip2));
    }
}

#[test]
fn should_handle_reorgs() {
    init_logger();
    let tmp_dir = TempDir::new().unwrap();
    let mut chain = chain(tmp_dir.path().into());
    let miner = Miner::new();

    // 1 block reorg

    let tip1 = chain.tip.clone();
    let tip2 = chain.tip.clone();

    let template1 = miner.create_block(tip1, None, &mut chain);
    let template2 = miner.create_block(tip2, None, &mut chain);

    let block1 = Miner::mine_block(template1, vec![]);
    let block2 = Miner::mine_block(template2, vec![]);

    let hash1 = block1.block_hash();
    let hash2 = block2.block_hash();

    chain.add(block1).unwrap();
    let tip3 = chain.add(block2).unwrap();

    assert_eq!(chain.tip.hash, hash1);

    // build on top of block 2
    let template3 = miner.create_block(tip3, None, &mut chain);
    let block3 = Miner::mine_block(template3, vec![]);
    let hash3 = block3.block_hash();

    // disconnect block 1, connect block 2 and 3
    chain.add(block3).unwrap();

    assert_eq!(chain.tip.hash, hash3);
    assert!(chain.db.is_main_hash(&hash2));

    // massive reorg
}
