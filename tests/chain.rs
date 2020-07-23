use maplit::hashmap;
use rust_bitcoin_node::protocol::{
    BIP8Deployment, BIP8ThresholdState, Deployment, NetworkParams, BASE_REWARD,
};
use rust_bitcoin_node::{
    blockchain::{Chain, ChainOptions},
    mining::Miner,
};
use std::{collections::HashMap, path::PathBuf};
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

fn bip8_activation(deployment: BIP8Deployment, expected_states: HashMap<u32, BIP8ThresholdState>) {
    let tmp_dir = TempDir::new().unwrap();
    let mut chain = chain(tmp_dir.path().into());
    let miner = Miner::new();

    let num_blocks = chain.options.network.miner_confirmation_window + deployment.timeout_height;

    chain
        .options
        .network
        .deployments
        .insert(deployment.name, Deployment::BIP8(deployment));

    // mine blocks until a difficulty adjustment period after the timeout height
    for _ in 0..num_blocks {
        let tip = chain.tip;
        let block_template = miner.create_block(tip, None, &mut chain);
        let block = Miner::mine_block(block_template, vec![]);

        assert!(chain.add(block).is_ok());
    }

    for (height, expected_status) in expected_states {
        let prev = chain.db.get_entry_by_height(height - 1).copied();
        assert_eq!(
            expected_status,
            chain.get_bip8_deployment_status(prev, deployment)
        );
    }
}

#[test]
fn test_bip8_activation() {
    bip8_activation(
        BIP8Deployment::new("taproot", 2, 144, 576, false),
        hashmap! {
            0 => BIP8ThresholdState::Defined,
            144 => BIP8ThresholdState::Started,
            576 => BIP8ThresholdState::Failing,
            576 + 144 => BIP8ThresholdState::Failed
        },
    );

    bip8_activation(
        BIP8Deployment::new("taproot", 2, 144, 576, true),
        hashmap! {
            0 => BIP8ThresholdState::Defined,
            144 => BIP8ThresholdState::Started,
            576 => BIP8ThresholdState::LockedIn,
            576 + 144 => BIP8ThresholdState::Active
        },
    );
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
