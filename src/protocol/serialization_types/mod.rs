// Copyright 2020 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0

//!
//! This crate provides a number of types that capture shape of the data structures used by Mina protocol
//! for communicating between nodes.
//!
//! When used with the serde enabled [bin-prot](https://crates.io/crates/bin-prot) crate
//! this allows for serializing and deserializing Mina protocol wire messages.
//!
//! This crate contains no code outside of autogenerated serde implementations. It is for reading serialized
//! data into strongly typed structures only.
//!

#![deny(warnings)]
#![deny(missing_docs)]

pub mod blockchain_state;
pub mod bulletproof_challenges;
pub mod common;
pub mod consensus_state;
pub mod delta_transition_chain_proof;
pub mod epoch_data;
pub mod errors;
pub mod external_transition;
pub mod field_and_curve_elements;
pub mod global_slot;
pub mod opening_proof;
pub mod proof_evaluations;
pub mod proof_messages;
pub mod protocol_constants;
pub mod protocol_state;
pub mod protocol_state_body;
pub mod protocol_state_proof;
pub mod protocol_version;
pub mod signatures;
pub mod snark_work;
pub mod staged_ledger_diff;
pub mod version_bytes;
