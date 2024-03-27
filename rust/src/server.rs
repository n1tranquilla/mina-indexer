use crate::{
    block::{self, parser::BlockParser, precomputed::PrecomputedBlock, BlockHash},
    constants::MAINNET_TRANSITION_FRONTIER_K,
    ledger::{
        genesis::GenesisLedger,
        staking::{self, StakingLedger},
        store::LedgerStore,
    },
    state::{IndexerState, IndexerStateConfig},
    store::IndexerStore,
    unix_socket_server::{self, UnixSocketServer},
};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::Arc,
};
use tokio::{
    runtime::Handle,
    sync::{mpsc, RwLock},
    task::JoinHandle,
};
use tracing::{debug, error, info, instrument, trace};

#[derive(Clone, Debug)]
pub struct IndexerConfiguration {
    pub genesis_ledger: GenesisLedger,
    pub genesis_hash: BlockHash,
    pub blocks_dir: PathBuf,
    pub block_watch_dir: PathBuf,
    pub ledgers_dir: PathBuf,
    pub ledger_watch_dir: PathBuf,
    pub prune_interval: u32,
    pub canonical_threshold: u32,
    pub canonical_update_threshold: u32,
    pub initialization_mode: InitializationMode,
    pub ledger_cadence: u32,
    pub reporting_freq: u32,
}

pub struct MinaIndexer {
    _witness_join_handle: JoinHandle<()>,
}

#[derive(Debug, Clone)]
pub enum InitializationMode {
    New,
    Replay,
    Sync,
}

impl MinaIndexer {
    pub async fn new(
        config: IndexerConfiguration,
        store: Arc<IndexerStore>,
        domain_socket_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let block_watch_dir = config.block_watch_dir.clone();
        let ledger_watch_dir = config.ledger_watch_dir.clone();

        let _witness_join_handle = tokio::spawn(async move {
            let state = initialize(config, store).await.unwrap_or_else(|e| {
                error!("Error in server initialization: {}", e);
                std::process::exit(1);
            });
            let state = Arc::new(RwLock::new(state));
            // Needs read-only state for summary
            unix_socket_server::start(UnixSocketServer::new(state.clone()), &domain_socket_path)
                .await;

            // This modifies the state
            if let Err(e) = run(block_watch_dir, ledger_watch_dir, state).await {
                error!("Error in server run: {}", e);
                std::process::exit(1);
            }
        });

        Ok(Self {
            _witness_join_handle,
        })
    }

    pub async fn await_loop(self) {
        let _ = self._witness_join_handle.await;
    }
}

async fn wait_for_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("failed to register signal handler");
    let mut int = signal(SignalKind::interrupt()).expect("failed to register signal handler");
    tokio::select! {
        _ = term.recv() => {
            trace!("Received SIGTERM");
            process::exit(100);
        },
        _ = int.recv() => {
            info!("Received SIGINT");
            process::exit(101);
        },
    }
}

async fn setup_signal_handler() {
    tokio::spawn(async move {
        let _ = wait_for_signal().await;
    });
}

pub async fn initialize(
    config: IndexerConfiguration,
    store: Arc<IndexerStore>,
) -> anyhow::Result<IndexerState> {
    info!("Starting mina-indexer server");
    setup_signal_handler().await;

    let db_path = store.db_path.clone();
    let IndexerConfiguration {
        genesis_ledger,
        genesis_hash,
        blocks_dir,
        ledgers_dir,
        prune_interval,
        canonical_threshold,
        canonical_update_threshold,
        initialization_mode,
        ledger_cadence,
        reporting_freq,
        ..
    } = config;

    fs::create_dir_all(blocks_dir.clone())?;
    fs::create_dir_all(ledgers_dir.clone())?;

    let state_config = IndexerStateConfig {
        genesis_hash,
        genesis_ledger: genesis_ledger.clone(),
        indexer_store: store,
        network: "mainnet".into(),
        transition_frontier_length: MAINNET_TRANSITION_FRONTIER_K,
        prune_interval,
        canonical_update_threshold,
        ledger_cadence,
        reporting_freq,
    };

    let mut state = match initialization_mode {
        InitializationMode::New => {
            info!(
                "Initializing indexer state from blocks in {} and staking ledgers in {}",
                blocks_dir.display(),
                ledgers_dir.display(),
            );
            IndexerState::new_from_config(state_config)?
        }
        InitializationMode::Replay => {
            info!("Replaying indexer events from db at {}", db_path.display());
            IndexerState::new_without_genesis_events(state_config)?
        }
        InitializationMode::Sync => {
            info!("Syncing indexer state from db at {}", db_path.display());
            IndexerState::new_without_genesis_events(state_config)?
        }
    };

    match initialization_mode {
        InitializationMode::New => {
            let mut block_parser = match BlockParser::new_with_canonical_chain_discovery(
                &blocks_dir,
                canonical_threshold,
                reporting_freq,
            ) {
                Ok(block_parser) => block_parser,
                Err(e) => {
                    panic!("Obtaining block parser failed: {e}");
                }
            };
            info!("Initializing indexer state");
            state
                .initialize_with_canonical_chain_discovery(&mut block_parser)
                .await?;
            state.add_startup_staking_ledgers_to_store(&ledgers_dir)?;
        }
        InitializationMode::Replay => {
            let min_length_filter = state.replay_events()?;
            let mut block_parser =
                BlockParser::new_length_sorted_min_filtered(&blocks_dir, min_length_filter)?;
            state.add_blocks(&mut block_parser).await?;
            state.add_startup_staking_ledgers_to_store(&ledgers_dir)?;
        }
        InitializationMode::Sync => {
            let min_length_filter = state.sync_from_db()?;
            let mut block_parser =
                BlockParser::new_length_sorted_min_filtered(&blocks_dir, min_length_filter)?;
            state.add_blocks(&mut block_parser).await?;
            state.add_startup_staking_ledgers_to_store(&ledgers_dir)?;
        }
    }
    Ok(state)
}

#[cfg(target_os = "linux")]
fn matches_event_kind(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Access(notify::event::AccessKind::Close(
            notify::event::AccessMode::Write
        )) | EventKind::Modify(notify::event::ModifyKind::Name(_))
    )
}

#[cfg(target_os = "macos")]
fn matches_event_kind(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content
        )) | EventKind::Modify(notify::event::ModifyKind::Name(_))
    )
}

#[instrument(skip_all)]
pub async fn run(
    block_watch_dir: impl AsRef<Path>,
    ledger_watch_dir: impl AsRef<Path>,
    state: Arc<RwLock<IndexerState>>,
) -> anyhow::Result<()> {
    // setup fs-based precomputed block & staking ledger watchers
    let (tx, mut rx) = mpsc::channel(4096);
    let rt = Handle::current();
    let mut watcher = RecommendedWatcher::new(
        move |result| {
            let tx = tx.clone();
            rt.spawn(async move {
                if let Err(e) = tx.send(result).await {
                    error!("Error sending event result: {}", e);
                }
            });
        },
        Config::default(),
    )?;

    watcher.watch(block_watch_dir.as_ref(), RecursiveMode::NonRecursive)?;
    info!(
        "Watching for new blocks in directory: {}",
        block_watch_dir.as_ref().display()
    );
    watcher.watch(ledger_watch_dir.as_ref(), RecursiveMode::NonRecursive)?;
    info!(
        "Watching for staking ledgers in directory: {}",
        ledger_watch_dir.as_ref().display()
    );

    // watch for precomputed blocks & staking ledgers
    while let Some(res) = rx.recv().await {
        match res {
            Ok(event) => {
                trace!("Event: {:?}", event.clone());
                if matches_event_kind(event.kind) {
                    for path in event.paths {
                        if block::is_valid_block_file(&path) {
                            debug!("Valid precomputed block file: {}", path.display());
                            match PrecomputedBlock::parse_file(&path) {
                                Ok(block) => {
                                    let mut state = state.write().await;
                                    match state.block_pipeline(&block) {
                                        Ok(_) => info!("Added block {}", block.summary()),
                                        Err(e) => error!("Error adding block: {}", e),
                                    }
                                }
                                Err(e) => error!("Error parsing precomputed block: {}", e),
                            }
                        } else if staking::is_valid_ledger_file(&path) {
                            let state = state.write().await;
                            if let Some(store) = state.indexer_store.as_ref() {
                                match StakingLedger::parse_file(&path) {
                                    Ok(staking_ledger) => {
                                        let ledger_summary = staking_ledger.summary();
                                        match store.add_staking_ledger(staking_ledger) {
                                            Ok(_) => {
                                                info!("Added staking ledger {}", ledger_summary);
                                            }
                                            Err(e) => error!("Error adding staking ledger: {}", e),
                                        }
                                    }
                                    Err(e) => {
                                        error!("Error parsing staking ledger: {}", e)
                                    }
                                }
                            } else {
                                error!("Indexer store unavailable");
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Block watcher error: {e:?}");
            }
        }
    }
    Ok(())
}