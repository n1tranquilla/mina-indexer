use crate::ledger::account::Amount;
use chrono::{DateTime, SecondsFormat, Utc};
use hex::ToHex;

// indexer constants

pub const BLOCK_REPORTING_FREQ_NUM: u32 = 1000;
pub const BLOCK_REPORTING_FREQ_SEC: u64 = 180;
pub const LEDGER_CADENCE: u32 = 100;
pub const CANONICAL_UPDATE_THRESHOLD: u32 = PRUNE_INTERVAL_DEFAULT / 5;
pub const MAINNET_CANONICAL_THRESHOLD: u32 = 10;
pub const PRUNE_INTERVAL_DEFAULT: u32 = 10;

// mina constants

pub const MAINNET_BLOCK_SLOT_TIME_MILLIS: u64 = 180000;
pub const MAINNET_GENESIS_HASH: &str = "3NKeMoncuHab5ScarV5ViyF16cJPT4taWNSaTLS64Dp67wuXigPZ";
pub const MAINNET_GENESIS_PREV_STATE_HASH: &str =
    "3NLoKn22eMnyQ7rxh5pxB6vBA3XhSAhhrf7akdqS6HbAKD14Dh1d";
pub const MAINNET_GENESIS_LAST_VRF_OUTPUT: &str = "NfThG1r1GxQuhaGLSJWGxcpv24SudtXG4etB0TnGqwg=";
pub const MAINNET_GENESIS_TIMESTAMP: u64 = 1615939200000;
pub const MAINNET_GENESIS_LEDGER_HASH: &str = "jx7buQVWFLsXTtzRgSxbYcT8EYLS8KCZbLrfDcJxMtyy4thw2Ee";
pub const MAINNET_TRANSITION_FRONTIER_K: u32 = 290;
pub const MAINNET_ACCOUNT_CREATION_FEE: Amount = Amount(1e9 as u64);
pub const MAINNET_COINBASE_REWARD: u64 = 720000000000;

// genesis constants

pub const MAINNET_GENESIS_CONSTANTS: &[u32] = &[
    MAINNET_TRANSITION_FRONTIER_K,
    MAINNET_EPOCH_SLOT_COUNT,
    MAINNET_SLOTS_PER_SUB_WINDOW,
    MAINNET_DELTA,
    MAINNET_TXPOOL_MAX_SIZE,
];
pub const MAINNET_EPOCH_SLOT_COUNT: u32 = 7140;
pub const MAINNET_SLOTS_PER_SUB_WINDOW: u32 = 7;
pub const MAINNET_DELTA: u32 = 0;
pub const MAINNET_TXPOOL_MAX_SIZE: u32 = 3000;

// constraint system digests

pub const MAINNET_CONSTRAINT_SYSTEM_DIGESTS: &[&str] = &[
    MAINNET_DIGEST_TXN_MERGE,
    MAINNET_DIGEST_TXN_BASE,
    MAINNET_DIGEST_BLOCKCHAIN_STEP,
];
pub const MAINNET_DIGEST_TXN_MERGE: &str = "d0f8e5c3889f0f84acac613f5c1c29b1";
pub const MAINNET_DIGEST_TXN_BASE: &str = "922bd415f24f0958d610607fc40ef227";
pub const MAINNET_DIGEST_BLOCKCHAIN_STEP: &str = "06d85d220ad13e03d51ef357d2c9d536";

/// Convert epoch milliseconds to an ISO 8601 formatted date
pub fn millis_to_iso_date_string(millis: i64) -> String {
    from_timestamp_millis(millis).to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Convert epoch milliseconds to DateTime<Utc>
fn from_timestamp_millis(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// Convert epoch milliseconds to global slot number
pub fn millis_to_global_slot(millis: i64) -> u64 {
    (millis as u64 - MAINNET_GENESIS_TIMESTAMP) / MAINNET_BLOCK_SLOT_TIME_MILLIS
}

/// Chain id used by mina node p2p network
pub fn chain_id(
    genesis_state_hash: &str,
    genesis_constants: &[u32],
    constraint_system_digests: &[&str],
) -> String {
    use blake2::{digest::VariableOutput, Blake2bVar};
    use std::io::Write;

    let genesis_constants_hash: String = {
        let mut gcs = genesis_constants
            .iter()
            .map(u32::to_string)
            .collect::<Vec<String>>();
        gcs.push(
            from_timestamp_millis(MAINNET_GENESIS_TIMESTAMP as i64)
                .format("%Y-%m-%d %H:%M:%S%.6fZ")
                .to_string(),
        );

        let mut hasher = Blake2bVar::new(32).unwrap();
        hasher.write_all(gcs.concat().as_bytes()).unwrap();
        hasher.finalize_boxed().encode_hex()
    };
    let all_snark_keys = constraint_system_digests.concat();
    let digest_str = [genesis_state_hash, &all_snark_keys, &genesis_constants_hash].concat();

    let mut hasher = Blake2bVar::new(32).unwrap();
    hasher.write_all(digest_str.as_bytes()).unwrap();
    hasher.finalize_boxed().to_vec().encode_hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_id_test() {
        assert_eq!(
            "5f704cc0c82e0ed70e873f0893d7e06f148524e3f0bdae2afb02e7819a0c24d1",
            chain_id(
                MAINNET_GENESIS_HASH,
                MAINNET_GENESIS_CONSTANTS,
                MAINNET_CONSTRAINT_SYSTEM_DIGESTS
            )
        )
    }
}
