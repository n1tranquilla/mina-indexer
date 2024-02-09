use crate::helpers::setup_new_db_dir;
use mina_indexer::{
    block::parser::BlockParser,
    ledger::genesis::GenesisRoot,
    state::{IndexerState, IndexerStateConfig},
    store::IndexerStore,
};
use std::{path::PathBuf, sync::Arc};

#[tokio::test]
async fn test() {
    let store_dir = setup_new_db_dir("event-replay").unwrap();
    let log_dir = PathBuf::from("./tests/data/canonical_chain_discovery/contiguous");
    let mut block_parser = BlockParser::new_testing(&log_dir).unwrap();
    let indexer_store = Arc::new(IndexerStore::new(store_dir.path()).unwrap());
    let genesis_contents = include_str!("../data/genesis_ledgers/mainnet.json");
    let genesis_ledger = serde_json::from_str::<GenesisRoot>(genesis_contents).unwrap();
    let mut state =
        IndexerState::new(genesis_ledger.clone().into(), indexer_store.clone(), 10).unwrap();

    // add all blocks to the state
    state.add_blocks(&mut block_parser).await.unwrap();

    // fresh state to replay events on top of
    let config = IndexerStateConfig::new(genesis_ledger.into(), indexer_store, 10);
    let mut new_state = IndexerState::new_without_genesis_events(config).unwrap();

    // replay events on the fresh state
    new_state.replay_events().unwrap();

    // witness trees match
    assert_eq!(state.best_tip_block(), new_state.best_tip_block());
    assert_eq!(state.canonical_tip_block(), new_state.canonical_tip_block());
    assert_eq!(state.diffs_map, new_state.diffs_map);
}
