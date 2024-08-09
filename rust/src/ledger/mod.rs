pub mod account;
pub mod coinbase;
pub mod diff;
pub mod genesis;
pub mod public_key;
pub mod staking;
pub mod store;
pub mod username;

use crate::{
    block::precomputed::PrecomputedBlock,
    constants::MAINNET_ACCOUNT_CREATION_FEE,
    ledger::{
        account::{Account, Amount, Nonce},
        diff::{account::AccountDiff, LedgerDiff},
        public_key::PublicKey,
    },
    protocol::serialization_types::{
        common::{Base58EncodableVersionedType, HashV1},
        version_bytes,
    },
};
use anyhow::bail;
use log::debug;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ops::{Add, Sub},
    str::FromStr,
};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Ledger {
    pub accounts: HashMap<PublicKey, Account>,
}

#[allow(clippy::len_without_is_empty)]
impl Ledger {
    pub fn len(&self) -> usize {
        self.accounts.len()
    }
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct NonGenesisLedger {
    pub ledger: Ledger,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LedgerHash(pub String);

impl LedgerHash {
    pub fn from_hashv1(hashv1: HashV1) -> Self {
        let versioned: Base58EncodableVersionedType<{ version_bytes::LEDGER_HASH }, _> =
            hashv1.into();
        Self(versioned.to_base58_string().unwrap())
    }

    pub fn from_bytes(bytes: Vec<u8>) -> anyhow::Result<Self> {
        let hash = String::from_utf8(bytes)?;
        if is_valid_ledger_hash(&hash) {
            Ok(Self(hash))
        } else {
            bail!("Invalid ledger hash: {hash}")
        }
    }
}

impl std::str::FromStr for LedgerHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if is_valid_ledger_hash(s) {
            Ok(Self(s.to_string()))
        } else {
            bail!("Invalid ledger hash: {s}")
        }
    }
}

impl std::default::Default for LedgerHash {
    fn default() -> Self {
        Self("jxDEFAULTDEFAULTDEFAULTDEFAULTDEFAULTDEFAULTDEFAULT".into())
    }
}

impl std::fmt::Display for LedgerHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Ledger {
    pub fn new() -> Self {
        Ledger {
            accounts: HashMap::new(),
        }
    }

    pub fn apply_diff_from_precomputed(self, block: &PrecomputedBlock) -> anyhow::Result<Self> {
        let diff = LedgerDiff::from_precomputed(block);
        self.apply_diff(&diff)
    }

    /// Apply a ledger diff
    pub fn apply_diff(self, diff: &LedgerDiff) -> anyhow::Result<Self> {
        let mut ledger = self;
        ledger._apply_diff(diff)?;
        Ok(ledger)
    }

    /// Apply a ledger diff to a mutable ledger
    pub fn _apply_diff(&mut self, diff: &LedgerDiff) -> anyhow::Result<()> {
        let keys: Vec<PublicKey> = diff
            .account_diffs
            .iter()
            .map(|diff| diff.public_key())
            .collect();
        keys.iter().for_each(|public_key| {
            if self.accounts.get(public_key).is_none() {
                self.accounts
                    .insert(public_key.clone(), Account::empty(public_key.clone()));
            }
        });

        for diff in diff.account_diffs.iter() {
            match self.accounts.remove(&diff.public_key()) {
                Some(account_before) => {
                    self.accounts.insert(
                        diff.public_key(),
                        match &diff {
                            AccountDiff::Payment(payment_diff) => {
                                Account::from_payment(account_before, payment_diff)
                            }
                            AccountDiff::CreateAccount(_) => account_before,
                            AccountDiff::Delegation(delegation_diff) => {
                                assert_eq!(account_before.public_key, delegation_diff.delegator);
                                Account::from_delegation(
                                    account_before.clone(),
                                    delegation_diff.delegate.clone(),
                                    delegation_diff.nonce,
                                )
                            }
                            AccountDiff::Coinbase(coinbase_diff) => {
                                Account::from_coinbase(account_before, coinbase_diff.amount)
                            }
                            AccountDiff::FeeTransfer(fee_transfer_diff) => {
                                Account::from_payment(account_before, fee_transfer_diff)
                            }
                            AccountDiff::FeeTransferViaCoinbase(fee_transfer_diff) => {
                                Account::from_payment(account_before, fee_transfer_diff)
                            }
                            AccountDiff::FailedTransactionNonce(failed_diff) => {
                                Account::from_failed_transaction(account_before, failed_diff.nonce)
                            }
                        },
                    );
                }
                None => {
                    return match diff {
                        AccountDiff::Coinbase(_) => Ok(()),
                        AccountDiff::Delegation(_) => bail!("Invalid delegation"),
                        AccountDiff::Payment(_)
                        | AccountDiff::CreateAccount(_)
                        | AccountDiff::FeeTransfer(_)
                        | AccountDiff::FeeTransferViaCoinbase(_)
                        | AccountDiff::FailedTransactionNonce(_) => {
                            bail!("Account {} not found", diff.public_key())
                        }
                    };
                }
            }
        }

        // account creation fees
        for pk in diff.new_pk_balances.keys() {
            match self.accounts.remove(pk) {
                Some(account_before) => {
                    self.accounts.insert(
                        pk.clone(),
                        Account {
                            balance: account_before.balance - MAINNET_ACCOUNT_CREATION_FEE,
                            ..account_before
                        },
                    );
                }
                None => unreachable!(),
            }
        }
        Ok(())
    }

    /// Unapply a ledger diff to a mutable ledger
    pub fn _unapply_diff(&mut self, diff: &LedgerDiff) -> anyhow::Result<()> {
        let keys: Vec<PublicKey> = diff
            .account_diffs
            .iter()
            .map(|diff| diff.public_key())
            .collect();
        keys.into_iter().for_each(|public_key| {
            if self.accounts.get(&public_key).is_none() {
                self.accounts
                    .insert(public_key.clone(), Account::empty(public_key));
            }
        });

        for diff in diff.account_diffs.iter() {
            match self.accounts.remove(&diff.public_key()) {
                Some(account_before) => {
                    self.accounts.insert(
                        diff.public_key(),
                        match &diff {
                            AccountDiff::Payment(payment_diff) => {
                                Account::from_payment_unapply(account_before, payment_diff)
                            }
                            AccountDiff::CreateAccount(pk) => {
                                assert!(self.accounts.get(pk).is_some());
                                self.accounts.remove(pk);
                                continue;
                            }
                            AccountDiff::Delegation(delegation_diff) => {
                                Account::from_delegation_unapply(
                                    account_before.clone(),
                                    // TODO get previous delegate?
                                    delegation_diff.delegate.clone(),
                                    Some(delegation_diff.nonce),
                                )
                            }
                            AccountDiff::Coinbase(coinbase_diff) => {
                                Account::from_coinbase(account_before, coinbase_diff.amount)
                            }
                            AccountDiff::FeeTransfer(fee_transfer_diff) => {
                                Account::from_payment_unapply(account_before, fee_transfer_diff)
                            }
                            AccountDiff::FeeTransferViaCoinbase(fee_transfer_diff) => {
                                Account::from_payment_unapply(account_before, fee_transfer_diff)
                            }
                            AccountDiff::FailedTransactionNonce(failed_diff) => {
                                Account::from_failed_transaction(account_before, failed_diff.nonce)
                            }
                        },
                    );
                }
                None => {
                    return match diff {
                        AccountDiff::Coinbase(_) => Ok(()),
                        AccountDiff::Delegation(_) => bail!("Invalid delegation"),
                        AccountDiff::Payment(_)
                        | AccountDiff::CreateAccount(_)
                        | AccountDiff::FeeTransfer(_)
                        | AccountDiff::FeeTransferViaCoinbase(_)
                        | AccountDiff::FailedTransactionNonce(_) => {
                            bail!("Account {} not found", diff.public_key())
                        }
                    };
                }
            }
        }
        Ok(())
    }

    pub fn time_locked_amount(&self, curr_global_slot: u32) -> Amount {
        Amount(
            self.accounts
                .values()
                .filter_map(|acct| {
                    acct.timing
                        .as_ref()
                        .map(|_| acct.current_minimum_balance(curr_global_slot))
                })
                .sum(),
        )
    }

    pub fn from(value: Vec<(&str, u64, Option<u32>, Option<&str>)>) -> anyhow::Result<Self> {
        let mut ledger = Ledger::new();
        for (pubkey, balance, nonce, delgation) in value {
            let pk = PublicKey::new(pubkey);
            let delegate = delgation.map(PublicKey::new).unwrap_or(pk.clone());
            ledger.accounts.insert(
                pk.clone(),
                Account {
                    delegate,
                    public_key: pk,
                    balance: balance.into(),
                    nonce: nonce.map(Nonce),
                    ..Default::default()
                },
            );
        }
        Ok(ledger)
    }

    pub fn to_string_pretty(&self) -> String {
        let mut accounts = HashMap::new();
        for (pk, acct) in &self.accounts {
            accounts.insert(pk.to_address(), acct.clone());
        }
        serde_json::to_string_pretty(&accounts).unwrap()
    }

    pub fn from_bytes(bytes: Vec<u8>) -> anyhow::Result<Self> {
        Self::from_str(&String::from_utf8(bytes.to_vec())?)
    }
}

impl ToString for Ledger {
    fn to_string(&self) -> String {
        let mut accounts = HashMap::new();
        for (pk, acct) in &self.accounts {
            accounts.insert(pk.to_address(), acct.clone());
        }
        serde_json::to_string(&accounts).unwrap()
    }
}

impl FromStr for Ledger {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let deser: HashMap<String, Account> = serde_json::from_str(s)?;
        let mut accounts = HashMap::new();
        for (pk, acct) in deser {
            accounts.insert(PublicKey(pk), acct);
        }
        Ok(Ledger { accounts })
    }
}

impl PartialEq for Ledger {
    fn eq(&self, other: &Self) -> bool {
        for pk in self.accounts.keys() {
            if self.accounts.get(pk) != other.accounts.get(pk) {
                debug!(
                    "[Ledger.eq mismatch] {pk:?} | {:?} | {:?}",
                    self.accounts.get(pk),
                    other.accounts.get(pk)
                );
                return false;
            }
        }
        for pk in other.accounts.keys() {
            if self.accounts.get(pk) != other.accounts.get(pk) {
                debug!(
                    "[Ledger.eq mismatch] {pk:?} | {:?} | {:?}",
                    self.accounts.get(pk),
                    other.accounts.get(pk)
                );
                return false;
            }
        }
        true
    }
}

impl Eq for Ledger {}

impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (pk, acct) in &self.accounts {
            writeln!(f, "{} -> {}", pk.to_address(), acct.balance.0)?;
        }
        writeln!(f)?;
        Ok(())
    }
}

impl Add<Amount> for Amount {
    type Output = Amount;

    fn add(self, rhs: Amount) -> Self::Output {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl Add<u64> for Amount {
    type Output = Amount;

    fn add(self, rhs: u64) -> Self::Output {
        Self(self.0.saturating_add(rhs))
    }
}

impl Add<i64> for Amount {
    type Output = Amount;

    fn add(self, rhs: i64) -> Self::Output {
        let abs = rhs.unsigned_abs();
        if rhs > 0 {
            Self(self.0.saturating_add(abs))
        } else {
            Self(self.0.saturating_sub(abs))
        }
    }
}

impl Sub<Amount> for Amount {
    type Output = Amount;

    fn sub(self, rhs: Amount) -> Self::Output {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl Sub<u64> for Amount {
    type Output = Amount;

    fn sub(self, rhs: u64) -> Self::Output {
        Self(self.0.saturating_sub(rhs))
    }
}

impl From<u64> for Amount {
    fn from(value: u64) -> Self {
        Amount(value)
    }
}

pub fn is_valid_ledger_hash(input: &str) -> bool {
    let mut chars = input.chars();
    let c0 = chars.next();
    let c1 = chars.next();
    input.len() == 51
        && c0 == Some('j')
        && (c1 == Some('w') || c1 == Some('x') || c1 == Some('y') || c1 == Some('z'))
}

#[cfg(test)]
mod tests {
    use super::{
        account::Account,
        diff::{
            account::{AccountDiff, DelegationDiff, PaymentDiff, UpdateType},
            LedgerDiff,
        },
        is_valid_ledger_hash,
        public_key::PublicKey,
        Ledger, LedgerHash,
    };
    use crate::{block::BlockHash, ledger::account::Nonce};
    use std::collections::{BTreeMap, HashMap};

    #[test]
    fn default_ledger_hash_is_valid_public_key() {
        assert!(is_valid_ledger_hash(&LedgerHash::default().0))
    }

    #[test]
    fn apply_diff_payment() {
        let diff_amount = 1.into();
        let public_key = PublicKey::new("B62qre3erTHfzQckNuibViWQGyyKwZseztqrjPZBv6SQF384Rg6ESAy");
        let account = Account::empty(public_key.clone());
        let mut accounts = HashMap::new();
        accounts.insert(public_key.clone(), account);

        let ledger_diff = LedgerDiff {
            blockchain_length: 0,
            state_hash: BlockHash::default(),
            new_pk_balances: BTreeMap::new(),
            new_coinbase_receiver: None,
            staged_ledger_hash: LedgerHash::default(),
            public_keys_seen: vec![],
            account_diffs: vec![AccountDiff::Payment(PaymentDiff {
                public_key: public_key.clone(),
                amount: diff_amount,
                update_type: UpdateType::Credit,
            })],
        };
        let ledger = Ledger { accounts }
            .apply_diff(&ledger_diff)
            .expect("ledger diff application");

        let account_after = ledger.accounts.get(&public_key).expect("account get");
        assert_eq!(account_after.public_key, public_key);
        assert_eq!(account_after.balance, diff_amount);
    }

    #[test]
    fn apply_diff_delegation() {
        let prev_nonce = Nonce(42);
        let public_key = PublicKey::new("B62qre3erTHfzQckNuibViWQGyyKwZseztqrjPZBv6SQF384Rg6ESAy");
        let delegate_key =
            PublicKey::new("B62qmMypEDCchUgPD6RU99gVKXJcY46urKdjbFmG5cYtaVpfKysXTz6");
        let account = Account::empty(public_key.clone());
        let mut accounts = HashMap::new();
        accounts.insert(public_key.clone(), account);

        let ledger_diff = LedgerDiff {
            blockchain_length: 0,
            state_hash: BlockHash::default(),
            new_pk_balances: BTreeMap::new(),
            new_coinbase_receiver: None,
            staged_ledger_hash: LedgerHash::default(),
            public_keys_seen: vec![],
            account_diffs: vec![AccountDiff::Delegation(DelegationDiff {
                delegator: public_key.clone(),
                delegate: delegate_key.clone(),
                nonce: prev_nonce + 1,
            })],
        };
        let ledger = Ledger { accounts }
            .apply_diff(&ledger_diff)
            .expect("ledger diff application");
        let account_after = ledger.accounts.get(&public_key).expect("account get");
        assert_eq!(account_after.public_key, public_key);
        assert_eq!(account_after.delegate, delegate_key);
        assert_eq!(Nonce(43), account_after.nonce.unwrap_or(Nonce(u32::MAX)));
    }
}
