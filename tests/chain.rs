use bitcoin::{OutPoint, Script, Transaction, TxIn, TxOut};
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
    Chain::new(ChainOptions {
        network: network_params,
        path,
        verify_scripts: true,
    })
    .unwrap()
}

fn init_logger() {
    let _ = env_logger::builder()
        .filter_module("rust_bitcoin_node", LevelFilter::Debug)
        .format_timestamp_millis()
        .is_test(true)
        .try_init();
}

fn create_tx(miner: &Miner, prev_tx: &Transaction) -> Transaction {
    let prev_output = &prev_tx.output[0 as usize];
    Transaction {
        version: prev_tx.version,
        lock_time: prev_tx.lock_time,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: prev_tx.txid(),
                vout: 0,
            },
            script_sig: Script::new(),
            sequence: 0xffffffff,
            witness: vec![],
        }],
        output: vec![TxOut {
            script_pubkey: miner.get_address().script_pubkey(),
            value: prev_output.value,
        }],
    }
}

fn get_coinbase_for_block_height(chain: &Chain, height: u32) -> Transaction {
    chain
        .db
        .get_block(chain.db.get_entry_by_height(height).unwrap().hash)
        .unwrap()
        .unwrap()
        .txdata[0]
        .clone()
}

// #[test]
// fn test_manual_tx() {
//     init_logger();
//     let tmp_dir = TempDir::new().unwrap();
//     let mut chain = chain(tmp_dir.path().into());

//     let mut mempool = MemPool::new();

//     let miner = Miner::new();

//     let tip = chain.tip;
//     let block_template = miner.create_block(tip, None, &mut chain);
//     let block = Miner::mine_block(block_template, vec![]);

//     assert!(chain.add(block).is_ok());

//     for _ in 0..115 {
//         let tip = chain.tip;
//         let block_template = miner.create_block(tip, None, &mut chain);
//         let block = Miner::mine_block(block_template, vec![]);

//         assert!(chain.add(block).is_ok());
//     }

//     let tip = chain.tip;
//     let block_template = miner.create_block(tip, None, &mut chain);

//     let tx1 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 1));
//     let tx2 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 2));
//     let tx3 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 3));
//     let tx4 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 4));
//     let tx5 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 5));
//     let tx6 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 6));
//     let tx7 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 7));
//     let tx8 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 8));
//     let tx9 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 9));
//     let tx10 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 10));
//     let tx11 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 11));
//     let tx12 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 12));
//     let tx13 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 13));
//     let tx14 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 14));
//     let tx15 = create_tx(&miner, &get_coinbase_for_block_height(&chain, 15));

//     mempool.add_tx(&chain, tx1.clone()).unwrap();
//     mempool.add_tx(&chain, tx2.clone()).unwrap();
//     mempool.add_tx(&chain, tx13.clone()).unwrap();

//     let block = Miner::mine_block(
//         block_template,
//         vec![
//             tx1, tx2, tx3, tx4, tx5, tx6, tx7, tx8, tx9, tx10, tx11, tx12, tx13, tx14, tx15,
//         ],
//     );

//     let hsid = HeaderAndShortIds::from_block(&block, 0, 2, &[]).unwrap();

//     let mut compactblock = CompactBlock::new(hsid).unwrap();

//     dbg!(&compactblock);

//     dbg!(compactblock.fill_mempool(&mempool));

//     let req = compactblock.into_block_transactions_request();

//     dbg!(&req);

//     let btxn = BlockTransactions::from_request(&req, &block).unwrap();

//     compactblock.fill_missing(btxn.transactions);

//     chain.add(compactblock.into_block()).unwrap();
// }

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
