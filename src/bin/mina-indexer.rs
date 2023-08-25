use clap::{Parser, Subcommand};
use mina_indexer::{
    client,
    server::{self, create_dir_if_non_existent, handle_command_line_arguments, MinaIndexer},
    store::IndexerStore,
};
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::prelude::*;

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
    Client {
        /// Output JSON data when possible
        #[arg(short, long, default_value_t = false)]
        output_json: bool,
        #[command(subcommand)]
        args: client::ClientCli,
    },
}

#[derive(Subcommand, Debug)]
enum ServerCommand {
    /// Start the mina indexer with a config file
    Config {
        #[arg(short, long)]
        path: PathBuf,
    },
    /// Start the mina indexer by passing in arguments manually on the command line
    Cli(server::ServerArgs),
}

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        IndexerCommand::Client { output_json, args } => client::run(&args, output_json).await,
        IndexerCommand::Server { server_command } => {
            let args = match server_command {
                ServerCommand::Cli(args) => args,
                ServerCommand::Config { path } => {
                    let config_file = tokio::fs::read(path).await?;
                    serde_yaml::from_reader(&config_file[..])?
                }
            };
            let option_snapshot_path = args.snapshot_path.clone();
            let database_dir = args.database_dir.clone();
            let log_dir = args.log_dir.clone();
            let log_level = args.log_level;
            let log_level_stdout = args.log_level_stdout;
            let config = handle_command_line_arguments(args).await?;

            let mut log_number = 0;
            let mut log_file = format!("{}/mina-indexer-{}.log", log_dir.display(), log_number);
            create_dir_if_non_existent(log_dir.to_str().unwrap()).await;
            while tokio::fs::metadata(&log_file).await.is_ok() {
                log_number += 1;
                log_file = format!("{}/mina-indexer-{}.log", log_dir.display(), log_number);
            }
            let log_file = PathBuf::from(log_file);

            // setup tracing
            if let Some(parent) = log_file.parent() {
                create_dir_if_non_existent(parent.to_str().unwrap()).await;
            }

            let log_file = std::fs::File::create(log_file.clone())?;
            let file_layer = tracing_subscriber::fmt::layer().with_writer(log_file);

            let stdout_layer = tracing_subscriber::fmt::layer();
            tracing_subscriber::registry()
                .with(stdout_layer.with_filter(log_level_stdout))
                .with(file_layer.with_filter(log_level))
                .init();

            let db = if let Some(snapshot_path) = option_snapshot_path {
                let indexer_store = IndexerStore::from_backup(&snapshot_path, &database_dir)?;
                Arc::new(indexer_store)
            } else {
                Arc::new(IndexerStore::new(&database_dir)?)
            };

            MinaIndexer::new(config, db.clone()).await?;
            mina_indexer::gql::start_gql(db).await.unwrap();
            Ok(())
        }
    }
}
