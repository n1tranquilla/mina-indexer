use self::precomputed::{BlockLogContents, PrecomputedBlock};
use anyhow::anyhow;
use mina_serialization_types::{common::Base58EncodableVersionedType, v1::HashV1, version_bytes};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, path::Path};

pub mod parser;
pub mod precomputed;
pub mod signed_command;
pub mod store;

#[derive(Hash, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct Block {
    pub parent_hash: BlockHash,
    pub state_hash: BlockHash,
    pub height: u32,
    pub blockchain_length: u32,
    pub global_slot_since_genesis: u32,
}

#[derive(Hash, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct BlockWithoutHeight {
    pub parent_hash: BlockHash,
    pub state_hash: BlockHash,
    pub blockchain_length: u32,
    pub global_slot_since_genesis: u32,
}

#[derive(Hash, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct BlockHash(pub String);

impl BlockHash {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        let block_hash = unsafe { String::from_utf8_unchecked(Vec::from(bytes)) };
        Self(block_hash)
    }

    pub fn from_hashv1(hashv1: HashV1) -> Self {
        let versioned: Base58EncodableVersionedType<{ version_bytes::STATE_HASH }, _> =
            hashv1.into();
        Self(versioned.to_base58_string().unwrap())
    }
}

impl Block {
    pub fn from_precomputed(precomputed_block: &PrecomputedBlock, height: u32) -> Self {
        let parent_hash =
            BlockHash::from_hashv1(precomputed_block.protocol_state.previous_state_hash.clone());
        let state_hash = BlockHash(precomputed_block.state_hash.clone());
        Self {
            parent_hash,
            state_hash,
            height,
            global_slot_since_genesis: precomputed_block
                .protocol_state
                .body
                .t
                .t
                .consensus_state
                .t
                .t
                .global_slot_since_genesis
                .t
                .t,
            blockchain_length: precomputed_block.blockchain_length,
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "{{ len: {}, state: {} }}",
            self.blockchain_length, self.state_hash.0
        )
    }
}

impl From<Block> for BlockWithoutHeight {
    fn from(value: Block) -> Self {
        Self {
            parent_hash: value.parent_hash.clone(),
            state_hash: value.state_hash.clone(),
            global_slot_since_genesis: value.global_slot_since_genesis,
            blockchain_length: value.blockchain_length,
        }
    }
}

impl BlockWithoutHeight {
    pub fn from_precomputed(precomputed_block: &PrecomputedBlock) -> Self {
        let parent_hash =
            BlockHash::from_hashv1(precomputed_block.protocol_state.previous_state_hash.clone());
        let state_hash = BlockHash(precomputed_block.state_hash.clone());
        Self {
            parent_hash,
            state_hash,
            global_slot_since_genesis: precomputed_block
                .protocol_state
                .body
                .t
                .t
                .consensus_state
                .t
                .t
                .global_slot_since_genesis
                .t
                .t,
            blockchain_length: precomputed_block.blockchain_length,
        }
    }
}

impl From<String> for BlockHash {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for BlockHash {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::cmp::PartialOrd for Block {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for Block {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.state_hash == other.state_hash {
            std::cmp::Ordering::Equal
        } else if self.height > other.height {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        }
    }
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Block {{ height: {}, len: {}, slot: {}, state: {}, parent: {} }}",
            self.height,
            self.blockchain_length,
            self.global_slot_since_genesis,
            &self.state_hash.0[0..12],
            &self.parent_hash.0[0..12]
        )
    }
}

impl std::fmt::Debug for BlockWithoutHeight {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Block {{ len: {}, slot: {}, state: {}, parent: {} }}",
            self.blockchain_length,
            self.global_slot_since_genesis,
            &self.state_hash.0[0..12],
            &self.parent_hash.0[0..12]
        )
    }
}

impl std::fmt::Debug for BlockHash {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "BlockHash {{ {:?} }}", self.0)
    }
}

impl std::fmt::Display for BlockHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parses the precomputed block if the path is a valid block file
pub fn parse_file(path: &Path) -> anyhow::Result<PrecomputedBlock> {
    if is_valid_block_file(path) {
        let file_name = path.file_name().expect("filename already checked");
        let blockchain_length = get_blockchain_length(file_name);
        let state_hash = get_state_hash(file_name).expect("state hash already checked");
        let log_file_contents = std::fs::read(path)?;
        let precomputed_block = PrecomputedBlock::from_log_contents(BlockLogContents {
            state_hash,
            blockchain_length,
            contents: log_file_contents,
        })?;
        Ok(precomputed_block)
    } else {
        Err(anyhow!(
            "Unable to parse invalid precomputed block: {}",
            path.display()
        ))
    }
}

/// Extracts a state hash from an OS file name
pub fn get_state_hash(file_name: &OsStr) -> Option<String> {
    let last_part = file_name.to_str()?.split('-').last()?.to_string();
    let state_hash = last_part.split('.').next()?;
    if state_hash.starts_with("3N") {
        return Some(state_hash.to_string());
    }
    None
}

/// Extracts a blockchain length from an OS file name
pub fn get_blockchain_length(file_name: &OsStr) -> Option<u32> {
    file_name
        .to_str()?
        .split('-')
        .fold(None, |acc, x| match x.parse::<u32>() {
            Err(_) => acc,
            Ok(x) => Some(x),
        })
}

pub fn is_valid_block_file(path: &Path) -> bool {
    fn is_valid_state_hash(input: &str) -> bool {
        input.starts_with("3N") && input.len() == 52
    }
    if let Some(ext) = path.extension() {
        // check json extension
        if ext.to_str() == Some("json") {
            // check file stem
            if let Some(file_name) = path.file_stem() {
                if let Some(parts) = file_name
                    .to_str()
                    .map(|name| name.split('-').collect::<Vec<&str>>())
                {
                    let is_valid_hash = parts
                        .last()
                        .map(|hash| is_valid_state_hash(hash))
                        .unwrap_or(false);
                    if parts.len() == 2 {
                        // e.g. mainnet-3NK2upcz2s6BmmoD6btjtJqSw1wNdyM9H5tXSD9nmN91mQMe4vH8.json
                        // check 2nd part is a state hash
                        return is_valid_hash;
                    } else if parts.len() == 3 {
                        // e.g. mainnet-2-3NLyWnjZqUECniE1q719CoLmes6WDQAod4vrTeLfN7XXJbHv6EHH.json
                        // check 2nd part is u32 and 3rd part is a state hash
                        let is_valid_length = parts.get(1).unwrap().parse::<u32>().is_ok();
                        return is_valid_hash && is_valid_length;
                    }
                }
            }
        }
    }
    false
}

pub fn length_from_path(path: &Path) -> Option<u32> {
    if is_valid_block_file(path) {
        get_blockchain_length(path.file_name()?)
    } else {
        None
    }
}
