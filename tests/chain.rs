use maplit::hashmap;
use rust_bitcoin_node::{
    blockchain::{Chain, ChainOptions},
    mining::Miner,
    protocol::StartTime,
    util::mock,
};
use rust_bitcoin_node::{
    protocol::{Deployment, NetworkParams, ThresholdState, Timeout, BASE_REWARD},
    util::now,
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
    env_logger::builder()
        .format_timestamp_millis()
        .is_test(true)
        .init();
}

fn speedy_trial_activation(
    deployment: Deployment,
    expected_states: HashMap<u32, ThresholdState>,
    signal: bool,
) {
    let tmp_dir = TempDir::new().unwrap();

    let mut network_params = NetworkParams::from_network(bitcoin::Network::Regtest);
    network_params
        .deployments
        .insert(deployment.name, deployment);

    let mut chain = Chain::new(ChainOptions {
        network: network_params.clone(),
        path: tmp_dir.path().into(),
        verify_scripts: true,
    })
    .unwrap();

    let miner = Miner::new();

    let timeout = match deployment.timeout {
        Timeout::NoTimeout => unreachable!(),
        Timeout::Timeout(timeout) => timeout,
    };

    // mine blocks until a difficulty adjustment period after the timeout or min_activation_height
    while (now() as u32) < (timeout + 144 * 10 * 60)
        || chain.height <= deployment.min_activation_height + 144
    {
        mock::advance_time(10 * 60);

        let tip = chain.tip;
        let mut block_template = miner.create_block(tip, None, &mut chain);

        if !signal {
            // reset version
            block_template.version = 4;
        }

        let block = Miner::mine_block(block_template, vec![]);

        chain.add(block).unwrap();
    }

    for (height, expected_status) in expected_states {
        let entry = *chain.db.get_entry_by_height(height).unwrap();
        let prev = chain.db.get_entry_by_hash(&entry.prev_block).copied();
        assert_eq!(
            expected_status,
            chain.get_deployment_status(prev, deployment)
        );
    }
}

#[test]
fn test_get_most_work_entry() {
    init_logger();
    mock::set_time(1296688602);
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

    let tip2 = chain.add(block1).unwrap();

    assert_eq!(chain.db.get_most_work_entry().unwrap().unwrap(), tip2);

    let tip3 = chain.add(block2).unwrap();

    // build on top of block 2
    let template3 = miner.create_block(tip3, None, &mut chain);
    let block3 = Miner::mine_block(template3, vec![]);

    // disconnect block 1, connect block 2 and 3
    let tip4 = chain.add(block3).unwrap();

    assert_eq!(chain.db.get_most_work_entry().unwrap().unwrap(), tip4);
}

#[test]
fn test_speedy_trial_activation() {
    init_logger();

    let start_time = 1296688602 as u32;

    let reset_time = || mock::set_time(start_time as u64);

    reset_time();

    // don't signal and timeout
    speedy_trial_activation(
        Deployment::new(
            "taproot",
            2,
            StartTime::StartTime(start_time),
            Timeout::Timeout(start_time + (10 * 60 * 144)),
            0,
        ),
        hashmap! {
            0 => ThresholdState::Defined,
            144 => ThresholdState::Started,
            288 => ThresholdState::Failed
        },
        false,
    );

    reset_time();

    // signal and activate
    speedy_trial_activation(
        Deployment::new(
            "taproot",
            2,
            StartTime::StartTime(start_time),
            Timeout::Timeout(start_time + (10 * 60 * 288)),
            0,
        ),
        hashmap! {
            0 => ThresholdState::Defined,
            144 => ThresholdState::Started,
            288 => ThresholdState::LockedIn,
            432 => ThresholdState::Active,
        },
        true,
    );

    reset_time();

    // signal but don't activate until diff adjustment after min_activation_height
    speedy_trial_activation(
        Deployment::new(
            "taproot",
            2,
            StartTime::StartTime(start_time),
            Timeout::Timeout(start_time + (10 * 60 * 288)),
            500,
        ),
        hashmap! {
            0 => ThresholdState::Defined,
            144 => ThresholdState::Started,
            288 => ThresholdState::LockedIn,
            500 => ThresholdState::LockedIn,
            576 => ThresholdState::Active,
        },
        true,
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
