use crate::{
    block::{parser::BlockParser, Block, BlockHash, BlockWithoutHeight},
    constants::{MAINNET_TRANSITION_FRONTIER_K, SOCKET_NAME},
    ipc::IpcActor,
    ledger::genesis::GenesisLedger,
    receiver::{filesystem::FilesystemReceiver, BlockReceiver},
    state::{summary::SummaryVerbose, IndexerState, IndexerStateConfig},
    store::IndexerStore,
};
use interprocess::local_socket::tokio::LocalSocketListener;
use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::Arc,
};
use tokio::{io, sync::mpsc, task::JoinHandle};
use tracing::{debug, info, instrument, trace};

#[derive(Clone, Debug)]
pub struct IndexerConfiguration {
    pub genesis_ledger: GenesisLedger,
    pub genesis_hash: BlockHash,
    pub startup_dir: PathBuf,
    pub watch_dir: PathBuf,
    pub prune_interval: u32,
    pub canonical_threshold: u32,
    pub canonical_update_threshold: u32,
    pub initialization_mode: InitializationMode,
    pub ledger_cadence: u32,
    pub reporting_freq: u32,
}

pub struct MinaIndexer {
    _ipc_join_handle: JoinHandle<()>,
    _witness_join_handle: JoinHandle<anyhow::Result<()>>,
}

#[derive(Debug)]
pub struct IpcChannelUpdate {
    pub best_tip: Block,
    pub summary: Box<SummaryVerbose>,
    pub store: Arc<IndexerStore>,
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
    ) -> anyhow::Result<Self> {
        let (ipc_update_sender, ipc_update_receiver) = mpsc::channel::<IpcChannelUpdate>(1);
        let ipc_update_arc = Arc::new(ipc_update_sender);
        let watch_dir = config.watch_dir.clone();
        let ipc_store = store.clone();

        let listener = LocalSocketListener::bind(SOCKET_NAME)
            .or_else(try_remove_old_socket)
            .unwrap_or_else(|e| panic!("unable to connect to domain socket: {:?}", e.to_string()));

        debug!("Local socket listener started");

        let _ipc_join_handle = tokio::spawn(async move {
            debug!("Spawning IPC Actor");

            let mut ipc_actor = IpcActor::new(listener, ipc_store, ipc_update_receiver);
            ipc_actor.run().await
        });
        let _witness_join_handle = tokio::spawn(async move {
            let state = initialize(config, store, ipc_update_arc.clone()).await?;
            run(watch_dir, state, ipc_update_arc.clone()).await
        });

        Ok(Self {
            _ipc_join_handle,
            _witness_join_handle,
        })
    }

    pub async fn await_loop(self) {
        let _ = self._witness_join_handle.await;
        let _ = self._ipc_join_handle.await;
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
    ipc_update_sender: Arc<mpsc::Sender<IpcChannelUpdate>>,
) -> anyhow::Result<IndexerState> {
    info!("Starting mina-indexer server");
    setup_signal_handler().await;

    let db_path = store.db_path.clone();
    let IndexerConfiguration {
        genesis_ledger,
        genesis_hash,
        startup_dir,
        prune_interval,
        canonical_threshold,
        canonical_update_threshold,
        initialization_mode,
        ledger_cadence,
        reporting_freq,
        ..
    } = config;

    fs::create_dir_all(startup_dir.clone()).expect("startup_dir created");

    let state_config = IndexerStateConfig {
        genesis_hash,
        genesis_ledger: genesis_ledger.clone(),
        indexer_store: store,
        transition_frontier_length: MAINNET_TRANSITION_FRONTIER_K,
        prune_interval,
        canonical_update_threshold,
        ledger_cadence,
        reporting_freq,
    };
    let state = {
        let mut state = match initialization_mode {
            InitializationMode::New => {
                info!(
                    "Initializing indexer state from blocks in {}",
                    startup_dir.display()
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

        let best_tip = state.best_tip_block().clone();
        let summary = Box::new(state.summary_verbose());
        let store = Arc::new(state.spawn_secondary_database()?);

        debug!("Updating IPC state");
        ipc_update_sender
            .send(IpcChannelUpdate {
                best_tip,
                summary,
                store,
            })
            .await?;

        match initialization_mode {
            InitializationMode::New => {
                let mut block_parser = match BlockParser::new_with_canonical_chain_discovery(
                    &startup_dir,
                    canonical_threshold,
                    reporting_freq,
                ) {
                    Ok(block_parser) => block_parser,
                    Err(e) => {
                        panic!("Obtaining block parser failed: {}", e);
                    }
                };
                info!("Initializing indexer state");
                state
                    .initialize_with_canonical_chain_discovery(&mut block_parser)
                    .await?;
            }
            InitializationMode::Replay => {
                let min_length_filter = state.replay_events()?;
                let mut block_parser =
                    BlockParser::new_glob_min_length_filtered(&startup_dir, min_length_filter)?;
                state.add_blocks(&mut block_parser).await?;
            }
            InitializationMode::Sync => {
                let min_length_filter = state.sync_from_db()?;
                let mut block_parser =
                    BlockParser::new_glob_min_length_filtered(&startup_dir, min_length_filter)?;
                state.add_blocks(&mut block_parser).await?;
            }
        }

        ipc_update_sender
            .send(IpcChannelUpdate {
                best_tip: state.best_tip_block().clone(),
                summary: Box::new(state.summary_verbose()),
                store: Arc::new(state.spawn_secondary_database()?),
            })
            .await?;
        state
    };

    Ok(state)
}

#[instrument(skip_all)]
pub async fn run(
    block_watch_dir: impl AsRef<Path>,
    mut state: IndexerState,
    ipc_update_sender: Arc<mpsc::Sender<IpcChannelUpdate>>,
) -> Result<(), anyhow::Error> {
    let mut filesystem_receiver = FilesystemReceiver::new(1024, 64).await?;
    filesystem_receiver.load_directory(block_watch_dir.as_ref())?;
    info!(
        "Block receiver set to watch {}",
        block_watch_dir.as_ref().to_path_buf().display()
    );

    loop {
        tokio::select! {
            block_fut = filesystem_receiver.recv_block() => {
                if let Some(precomputed_block) = block_fut? {
                    let block = BlockWithoutHeight::from_precomputed(&precomputed_block);
                    debug!("Receiving block (length {}): {}", block.blockchain_length, block.state_hash);

                    if state.block_pipeline(&precomputed_block)? {
                        info!("Added block (length {}): {}", block.blockchain_length, block.state_hash);
                    }

                    ipc_update_sender.send(IpcChannelUpdate {
                        best_tip: state.best_tip_block().clone(),
                        summary: Box::new(state.summary_verbose()),
                        store: Arc::new(state.spawn_secondary_database()?),
                    }).await?;
                } else {
                    info!("Block receiver shutdown, system exit");
                    return Ok(())
                }
            }
        }
    }
}

fn try_remove_old_socket(e: io::Error) -> io::Result<LocalSocketListener> {
    if e.kind() == io::ErrorKind::AddrInUse {
        debug!(
            "Domain socket: {} already in use. Removing old vestige",
            &SOCKET_NAME
        );
        remove_domain_socket()?;
        LocalSocketListener::bind(SOCKET_NAME)
    } else {
        Err(e)
    }
}

pub fn remove_domain_socket() -> io::Result<()> {
    std::fs::remove_file(SOCKET_NAME)?;
    debug!("Domain socket removed: {SOCKET_NAME}");
    Ok(())
}
