use std::{path::PathBuf, process, sync::Arc};

use futures_util::{io::BufReader, AsyncBufReadExt, AsyncWriteExt};
use interprocess::local_socket::tokio::{LocalSocketListener, LocalSocketStream};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, instrument, trace};

use crate::{
    block::{store::BlockStore, Block, BlockHash},
    canonical::store::CanonicityStore,
    ledger::{public_key::PublicKey, store::LedgerStore, Ledger},
    server::{IndexerConfiguration, IpcChannelUpdate},
    state::summary::{SummaryShort, SummaryVerbose},
    store::IndexerStore,
};

#[derive(Debug)]
pub struct IpcActor {
    state_recv: IpcStateReceiver,
    listener: LocalSocketListener,
    best_tip: RwLock<Block>,
    ledger: RwLock<Ledger>,
    summary: RwLock<Option<SummaryVerbose>>,
    store: RwLock<Arc<IndexerStore>>,
}
type IpcStateReceiver = mpsc::Receiver<IpcChannelUpdate>;
#[derive(Debug, Hash, PartialEq, Eq)]
pub enum IpcActorError {}

impl IpcActor {
    #[instrument(skip_all)]
    pub fn new(
        config: IndexerConfiguration,
        listener: LocalSocketListener,
        store: Arc<IndexerStore>,
        state_recv: IpcStateReceiver,
    ) -> Self {
        info!("Creating new IPC actor");
        Self {
            state_recv,
            listener,
            best_tip: RwLock::new(Block {
                parent_hash: config.root_hash.clone(),
                state_hash: config.root_hash,
                height: 1,
                blockchain_length: 1,
                global_slot_since_genesis: 0,
            }),
            ledger: RwLock::new(config.ledger.ledger.into()),
            summary: RwLock::new(None),
            store: RwLock::new(store),
        }
    }

    #[instrument(skip(self))]
    pub async fn run(&mut self) -> () {
        loop {
            tokio::select! {
                state = self.state_recv.recv() => {
                    debug!("Received IPC state update");
                    match state {
                        None => panic!("IPC channel closed"),
                        Some(state) => {
                            debug!("Setting IPC state");
                            *self.best_tip.write().await = state.best_tip;
                            *self.ledger.write().await = state.ledger;
                            *self.summary.write().await = Some(*state.summary);
                            *self.store.write().await = state.store;
                        },
                    }
                }

                client = self.listener.accept() => {
                    let store = self.store.read().await.clone();
                    let best_tip = self.best_tip.read().await.clone();
                    let ledger = self.ledger.read().await.clone();
                    let summary = self.summary.read().await.clone();
                    match client {
                        Err(e) => error!("Error accepting connection: {}", e.to_string()),
                        Ok(stream) => {
                            info!("Accepted client connection");
                            tokio::spawn(async move {
                                debug!("Handling client connection");
                                match handle_conn(stream,
                                    &store,
                                    &best_tip,
                                    &ledger,
                                    summary.as_ref()
                                ).await {
                                    Err(e) => {
                                        error!("Error handling connection: {e}");
                                    },
                                    Ok(_) => { info!("handled connection"); },
                                };
                                debug!("Removing readonly instance at {}", store.db_path.clone().display());
                                tokio::fs::remove_dir_all(&store.db_path).await.ok();
                            });
                        }
                    }
                }
            }
        }
    }
}

#[instrument(skip_all)]
async fn handle_conn(
    conn: LocalSocketStream,
    db: &IndexerStore,
    best_tip: &Block,
    ledger: &Ledger,
    summary: Option<&SummaryVerbose>,
) -> Result<(), anyhow::Error> {
    use anyhow::anyhow;
    let (reader, mut writer) = conn.into_split();
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::with_capacity(1024);
    let read_size = reader.read_until(0, &mut buffer).await?;

    if read_size == 0 {
        return Err(anyhow!("Unexpected EOF"));
    }

    let mut buffers = buffer.split(|byte| *byte == b' ');
    let command = buffers.next().unwrap();
    let command_string = String::from_utf8(command.to_vec()).unwrap();
    let response_json = match command_string.as_str() {
        "account" => {
            let data_buffer = buffers.next().unwrap();
            let public_key = PublicKey::from_address(&String::from_utf8(
                data_buffer[..data_buffer.len() - 1].to_vec(),
            )?)?;
            info!("Received account command for {public_key:?}");
            trace!("Using ledger {ledger:?}");
            let account = ledger.accounts.get(&public_key);
            if let Some(account) = account {
                debug!("Writing account {account:?} to client");
                Some(serde_json::to_string(account)?)
            } else {
                None
            }
        }
        "best_chain" => {
            info!("Received best_chain command");
            let data_buffer = buffers.next().unwrap();
            let num = String::from_utf8(data_buffer[..data_buffer.len() - 1].to_vec())?
                .parse::<usize>()?;
            let mut parent_hash = best_tip.parent_hash.clone();
            let mut best_chain = vec![db.get_block(&best_tip.state_hash)?.unwrap()];
            for _ in 1..num {
                let parent_pcb = db.get_block(&parent_hash)?.unwrap();
                parent_hash =
                    BlockHash::from_hashv1(parent_pcb.protocol_state.previous_state_hash.clone());
                best_chain.push(parent_pcb);
            }
            Some(serde_json::to_string(&best_chain)?)
        }
        "best_ledger" => {
            info!("Received best_ledger command");
            let ledger = ledger.to_string();
            match buffers.next() {
                Some(data_buffer) => {
                    let data = String::from_utf8(data_buffer[..data_buffer.len() - 1].to_vec())?;
                    if data.is_empty() {
                        debug!("Writing best ledger to stdout");
                        Some(ledger)
                    } else {
                        let path = &data.parse::<PathBuf>()?;
                        if !path.is_dir() {
                            debug!("Writing best ledger to {}", path.display());
                            tokio::fs::write(path, ledger).await?;
                            Some(format!("Best ledger written to {}", path.display()))
                        } else {
                            Some(serde_json::to_string(&format!(
                                "The path provided must not be a directory: {}",
                                path.display()
                            ))?)
                        }
                    }
                }
                _ => None,
            }
        }
        "ledger" => {
            let hash_buffer = buffers.next().unwrap();
            let hash = String::from_utf8(hash_buffer[..hash_buffer.len() - 1].to_vec())?;
            info!("Received ledger command for {hash}");

            if let Some(ledger) = db.get_ledger(&hash.clone().into())? {
                let ledger = ledger.to_string();
                match buffers.next() {
                    None => {
                        debug!("Writing ledger at {hash} to stdout");
                        Some(ledger)
                    }
                    Some(path_buffer) => {
                        let path =
                            &String::from_utf8(path_buffer[..path_buffer.len() - 1].to_vec())?
                                .parse::<PathBuf>()?;
                        if !path.is_dir() {
                            debug!("Writing ledger at {hash} to {}", path.display());
                            tokio::fs::write(path, ledger).await?;
                            Some(serde_json::to_string(&format!(
                                "Ledger at {hash} written to {}",
                                path.display()
                            ))?)
                        } else {
                            Some(serde_json::to_string(&format!(
                                "The path provided must not be a directory: {}",
                                path.display()
                            ))?)
                        }
                    }
                }
            } else {
                Some(format!(
                    "Invalid query: ledger at {hash} cannot be determined"
                ))
            }
        }
        "ledger_at_height" => {
            let height_buffer = buffers.next().unwrap();
            let height = String::from_utf8(height_buffer[..height_buffer.len() - 1].to_vec())?
                .parse::<u32>()?;
            info!("Received ledger_at_height {height} command");

            if height > db.get_max_canonical_blockchain_length()?.unwrap_or(0) {
                Some(format!("Invalid query: ledger at height {height} cannot be determined from chain of length {}", best_tip.blockchain_length))
            } else if let Some(ledger) = db.get_ledger_at_height(height)? {
                let ledger = ledger.to_string();
                match buffers.next() {
                    None => {
                        debug!("Writing ledger at height {height} to stdout");
                        Some(ledger)
                    }
                    Some(path_buffer) => {
                        let path =
                            &String::from_utf8(path_buffer[..path_buffer.len() - 1].to_vec())?
                                .parse::<PathBuf>()?;
                        if !path.is_dir() {
                            debug!("Writing ledger at height {height} to {}", path.display());
                            tokio::fs::write(&path, ledger).await?;
                            Some(serde_json::to_string(&format!(
                                "Ledger at height {height} written to {}",
                                path.display()
                            ))?)
                        } else {
                            Some(serde_json::to_string(&format!(
                                "The path provided must not be a directory: {}",
                                path.display()
                            ))?)
                        }
                    }
                }
            } else {
                None
            }
        }
        "summary" => {
            info!("Received summary command");
            let data_buffer = buffers.next().unwrap();
            let verbose = String::from_utf8(data_buffer[..data_buffer.len() - 1].to_vec())?
                .parse::<bool>()?;
            if let Some(summary) = summary {
                if verbose {
                    Some(serde_json::to_string::<SummaryVerbose>(summary)?)
                } else {
                    Some(serde_json::to_string::<SummaryShort>(
                        &summary.clone().into(),
                    )?)
                }
            } else {
                Some(serde_json::to_string(&String::from(
                    "No summary available yet",
                ))?)
            }
        }
        "shutdown" => {
            info!("Received shutdown command");
            writer
                .write_all(b"Shutting down the Mina Indexer daemon...")
                .await?;
            info!("Shutting down the indexer...");
            process::exit(0);
        }
        bad_request => {
            return Err(anyhow!("Malformed request: {bad_request}"));
        }
    };

    if let Some(response_json) = response_json {
        writer.write_all(response_json.as_bytes()).await?;
    } else {
        writer
            .write_all(serde_json::to_string("no response 404")?.as_bytes())
            .await?;
    }

    Ok(())
}
