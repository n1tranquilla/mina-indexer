use clap::{Parser, Subcommand};
use mina_indexer::{
    client,
    constants::*,
    ledger::{self, genesis::GenesisLedger},
    server::{IndexerConfiguration, InitializationMode, MinaIndexer},
    store::IndexerStore,
};
use std::{fs, path::PathBuf, sync::Arc};
use tracing::{error, info, instrument};
use tracing_subscriber::{filter::LevelFilter, prelude::*};

#[derive(Parser, Debug)]
#[command(name = "mina-indexer", author, version, about, long_about = Some("Mina Indexer\n\n\
Efficiently index and query the Mina blockchain"))]
struct Cli {
    #[command(subcommand)]
    command: IndexerCommand,
}

#[derive(Subcommand, Debug)]
enum IndexerCommand {
    /// Server commands
    Server {
        #[command(subcommand)]
        server_command: ServerCommand,
    },
    /// Client commands
    #[clap(flatten)]
    Client(#[command(subcommand)] client::ClientCli),
}

#[derive(Subcommand, Debug)]
enum ServerCommand {
    /// Start a new mina indexer by passing arguments on the command line
    Start(ServerArgs),
    /// Start a mina indexer by replaying events from an existing indexer store
    Replay(ServerArgs),
    /// Start a mina indexer by syncing from events in an existing indexer store
    Sync(ServerArgs),
}

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct ServerArgs {
    /// Path to the genesis ledger
    #[arg(
        short,
        long,
        default_value = concat!(env!("PWD"), "/tests/data/genesis_ledgers/mainnet.json")
    )]
    genesis_ledger: PathBuf,
    /// Hash of the initial state
    #[arg(
        long,
        default_value = MAINNET_GENESIS_HASH
    )]
    genesis_hash: String,
    /// Path to startup blocks directory
    #[arg(short, long, default_value = concat!(env!("HOME"), "/.mina-indexer/startup-blocks"))]
    startup_dir: PathBuf,
    /// Path to directory to watch for new blocks [default: startup_dir]
    #[arg(short, long)]
    watch_dir: Option<PathBuf>,
    /// Path to directory for speedb
    #[arg(short, long, default_value = concat!(env!("HOME"), "/.mina-indexer/database"))]
    pub database_dir: PathBuf,
    /// Path to directory for logs
    #[arg(long, default_value = concat!(env!("HOME"), "/.mina-indexer/logs"))]
    pub log_dir: PathBuf,
    /// Max stdout log level
    #[arg(long, default_value_t = LevelFilter::INFO)]
    pub log_level: LevelFilter,
    /// Max file log level
    #[arg(long, default_value_t = LevelFilter::DEBUG)]
    pub log_level_file: LevelFilter,
    /// Cadence for computing and storing ledgers
    #[arg(long, default_value_t = LEDGER_CADENCE)]
    ledger_cadence: u32,
    /// Cadence for reporting progress
    #[arg(long, default_value_t = BLOCK_REPORTING_FREQ_NUM)]
    reporting_freq: u32,
    /// Interval for pruning the root branch
    #[arg(short, long, default_value_t = PRUNE_INTERVAL_DEFAULT)]
    prune_interval: u32,
    /// Threshold for determining the canonicity of a block
    #[arg(long, default_value_t = MAINNET_CANONICAL_THRESHOLD)]
    canonical_threshold: u32,
    /// Threshold for updating the canonical tip/ledger
    #[arg(long, default_value_t = CANONICAL_UPDATE_THRESHOLD)]
    canonical_update_threshold: u32,
    /// Web server hostname for REST and GraphQL
    #[arg(long, default_value = "localhost")]
    web_hostname: String,
    /// Web server port for REST and GraphQL
    #[arg(long, default_value_t = 8080)]
    web_port: u16,
    /// Path to the genesis ledger
    #[arg(long, default_value = concat!(env!("PWD"), "/data/locked.csv"))]
    locked_supply_csv: PathBuf,
}

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        IndexerCommand::Client(args) => client::run(&args).await,
        IndexerCommand::Server { server_command } => {
            let (args, mut mode) = match server_command {
                ServerCommand::Start(args) => (args, InitializationMode::New),
                ServerCommand::Sync(args) => (args, InitializationMode::Sync),
                ServerCommand::Replay(args) => (args, InitializationMode::Replay),
            };
            let locked_supply_csv = args.locked_supply_csv.clone();
            let database_dir = args.database_dir.clone();
            let web_hostname = args.web_hostname.clone();
            let web_port = args.web_port;

            if let Ok(dir) = std::fs::read_dir(database_dir.clone()) {
                if matches!(mode, InitializationMode::New) && dir.count() != 0 {
                    // sync from existing db
                    mode = InitializationMode::Sync;
                }
            }

            let log_dir = args.log_dir.clone();
            let log_level_file = args.log_level_file;
            let log_level = args.log_level;

            init_tracing_logger(log_dir.clone(), log_level_file, log_level).await?;

            // write server args to config.json
            let path = log_dir.with_file_name("config.json");
            let args_json: ServerArgsJson = args.clone().into();
            std::fs::write(path, serde_json::to_string_pretty(&args_json)?)?;

            let config = process_indexer_configuration(args, mode)?;
            let db = Arc::new(IndexerStore::new(&database_dir)?);
            let indexer = MinaIndexer::new(config, db.clone()).await?;

            mina_indexer::web::start_web_server(db, (web_hostname, web_port), locked_supply_csv)
                .await
                .unwrap();
            indexer.await_loop().await;
            Ok(())
        }
    }
}

async fn init_tracing_logger(
    log_dir: PathBuf,
    log_level: LevelFilter,
    log_level_stdout: LevelFilter,
) -> anyhow::Result<()> {
    let mut log_number = 0;
    let mut log_file = format!("{}/mina-indexer-{}.log", log_dir.display(), log_number);
    fs::create_dir_all(log_dir.clone()).expect("log_dir should be created");

    while tokio::fs::metadata(&log_file).await.is_ok() {
        log_number += 1;
        log_file = format!("{}/mina-indexer-{}.log", log_dir.display(), log_number);
    }
    let log_file = PathBuf::from(log_file);

    // setup tracing
    if let Some(parent) = log_file.parent() {
        fs::create_dir_all(parent).expect("log_file parent should be created");
    }

    let log_file = std::fs::File::create(log_file)?;
    let file_layer = tracing_subscriber::fmt::layer().with_writer(log_file);

    let stdout_layer = tracing_subscriber::fmt::layer();
    tracing_subscriber::registry()
        .with(stdout_layer.with_filter(log_level_stdout))
        .with(file_layer.with_filter(log_level))
        .init();
    Ok(())
}

#[instrument(skip_all)]
pub fn process_indexer_configuration(
    args: ServerArgs,
    mode: InitializationMode,
) -> anyhow::Result<IndexerConfiguration> {
    let ledger = args.genesis_ledger;
    let genesis_hash = args.genesis_hash.into();
    let startup_dir = args.startup_dir;
    let watch_dir = args.watch_dir.unwrap_or(startup_dir.clone());
    let prune_interval = args.prune_interval;
    let canonical_threshold = args.canonical_threshold;
    let canonical_update_threshold = args.canonical_update_threshold;
    let ledger_cadence = args.ledger_cadence;
    let reporting_freq = args.reporting_freq;

    assert!(
        ledger.is_file(),
        "Ledger file does not exist at {}",
        ledger.display()
    );
    assert!(
        // bad things happen if this condition fails
        canonical_update_threshold < MAINNET_TRANSITION_FRONTIER_K,
        "canonical update threshold must be strictly less than the transition frontier length!"
    );
    fs::create_dir_all(watch_dir.clone()).expect("watch_dir should be created");

    info!("Parsing ledger file at {}", ledger.display());

    match ledger::genesis::parse_file(&ledger) {
        Err(err) => {
            error!("Unable to parse genesis ledger: {err}");
            std::process::exit(100)
        }
        Ok(genesis_root) => {
            let genesis_ledger: GenesisLedger = genesis_root.into();
            info!("Genesis ledger parsed successfully");

            Ok(IndexerConfiguration {
                genesis_ledger,
                genesis_hash,
                startup_dir,
                watch_dir,
                prune_interval,
                canonical_threshold,
                canonical_update_threshold,
                initialization_mode: mode,
                ledger_cadence,
                reporting_freq,
            })
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ServerArgsJson {
    genesis_ledger: String,
    genesis_hash: String,
    startup_dir: String,
    watch_dir: String,
    database_dir: String,
    log_dir: String,
    log_level: String,
    log_level_file: String,
    ledger_cadence: u32,
    reporting_freq: u32,
    prune_interval: u32,
    canonical_threshold: u32,
    canonical_update_threshold: u32,
}

impl From<ServerArgs> for ServerArgsJson {
    fn from(value: ServerArgs) -> Self {
        Self {
            genesis_ledger: value.genesis_ledger.display().to_string(),
            genesis_hash: value.genesis_hash,
            startup_dir: value.startup_dir.display().to_string(),
            watch_dir: value
                .watch_dir
                .unwrap_or(value.startup_dir)
                .display()
                .to_string(),
            database_dir: value.database_dir.display().to_string(),
            log_dir: value.log_dir.display().to_string(),
            log_level: value.log_level.to_string(),
            log_level_file: value.log_level_file.to_string(),
            ledger_cadence: value.ledger_cadence,
            reporting_freq: value.reporting_freq,
            prune_interval: value.prune_interval,
            canonical_threshold: value.canonical_threshold,
            canonical_update_threshold: value.canonical_update_threshold,
        }
    }
}
