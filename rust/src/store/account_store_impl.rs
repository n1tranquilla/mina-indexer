use super::{account::AccountBalanceUpdate, column_families::ColumnFamilyHelpers, from_be_bytes};
use crate::{
    block::{store::BlockStore, BlockHash},
    constants::MAINNET_GENESIS_HASH,
    ledger::public_key::PublicKey,
    store::{
        account::{AccountStore, DBAccountBalanceUpdate},
        fixed_keys::FixedKeys,
        u64_prefix_key, IndexerStore,
    },
};
use log::trace;
use speedb::{DBIterator, IteratorMode};

impl AccountStore for IndexerStore {
    fn reorg_account_balance_updates(
        &self,
        old_best_tip: &BlockHash,
        new_best_tip: &BlockHash,
    ) -> anyhow::Result<DBAccountBalanceUpdate> {
        trace!(
            "Getting common ancestor account balance updates:\n  old: {}\n  new: {}",
            old_best_tip,
            new_best_tip
        );

        // follows the old best tip back to the common ancestor
        let mut a = old_best_tip.clone();
        let mut unapply = vec![];

        // follows the new best tip back to the common ancestor
        let mut b = new_best_tip.clone();
        let mut apply = vec![];

        let a_length = self.get_block_height(&a)?.expect("a has a length");
        let b_length = self.get_block_height(&b)?.expect("b has a length");

        // bring b back to the same height as a
        let genesis_state_hashes: Vec<BlockHash> = vec![MAINNET_GENESIS_HASH.into()];
        for _ in 0..b_length.saturating_sub(a_length) {
            // check if there's a previous block
            if genesis_state_hashes.contains(&b) {
                break;
            }

            apply.append(&mut self.get_block_balance_updates(&b)?.unwrap());
            b = self.get_block_parent_hash(&b)?.expect("b has a parent");
        }

        // find the common ancestor
        let mut a_prev = self.get_block_parent_hash(&a)?.expect("a has a parent");
        let mut b_prev = self.get_block_parent_hash(&b)?.expect("b has a parent");

        while a != b && !genesis_state_hashes.contains(&a) {
            // add blocks to appropriate collection
            unapply.append(&mut self.get_block_balance_updates(&a)?.unwrap());
            apply.append(&mut self.get_block_balance_updates(&b)?.unwrap());

            // descend
            a = a_prev;
            b = b_prev;

            a_prev = self.get_block_parent_hash(&a)?.expect("a has a parent");
            b_prev = self.get_block_parent_hash(&b)?.expect("b has a parent");
        }

        apply.reverse();
        Ok(<DBAccountBalanceUpdate>::new(apply, unapply))
    }

    fn get_block_balance_updates(
        &self,
        state_hash: &BlockHash,
    ) -> anyhow::Result<Option<Vec<AccountBalanceUpdate>>> {
        trace!("Getting block balance updates for {state_hash}");
        Ok(self
            .database
            .get_pinned_cf(self.account_balance_updates_cf(), state_hash.0.as_bytes())?
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
    }

    fn update_account_balances(
        &self,
        state_hash: &BlockHash,
        updates: &DBAccountBalanceUpdate,
    ) -> anyhow::Result<()> {
        trace!("Updating account balances {state_hash}");

        use AccountBalanceUpdate::*;
        fn count(updates: &[AccountBalanceUpdate]) -> i32 {
            updates.iter().fold(0, |acc, update| match update {
                CreateAccount(_) => acc + 1,
                RemoveAccount(_) => acc - 1,
                Payment(_) => acc,
            })
        }
        self.update_num_accounts(count(&updates.apply) - count(&updates.unapply))?;

        // update balances
        for (pk, amount) in <DBAccountBalanceUpdate>::balance_updates(updates) {
            if let Some(amount) = amount {
                let balance = self.get_account_balance(&pk)?.unwrap_or_default();
                let balance = if amount > 0 {
                    balance + amount.unsigned_abs()
                } else {
                    balance.saturating_sub(amount.unsigned_abs())
                };

                // update balance
                self.update_account_balance(&pk, Some(balance))?;
            } else {
                // remove account
                self.update_account_balance(&pk, None)?;
            }
        }
        Ok(())
    }

    fn update_num_accounts(&self, adjust: i32) -> anyhow::Result<()> {
        use std::cmp::Ordering::*;

        match adjust.cmp(&0) {
            Equal => (),
            Greater => {
                let old = self
                    .database
                    .get(Self::TOTAL_NUM_ACCOUNTS_KEY)?
                    .map_or(0, from_be_bytes);
                self.database.put(
                    Self::TOTAL_NUM_ACCOUNTS_KEY,
                    old.saturating_add(adjust.unsigned_abs()).to_be_bytes(),
                )?;
            }
            Less => {
                let old = self
                    .database
                    .get(Self::TOTAL_NUM_ACCOUNTS_KEY)?
                    .map_or(0, from_be_bytes);
                self.database.put(
                    Self::TOTAL_NUM_ACCOUNTS_KEY,
                    old.saturating_sub(adjust.unsigned_abs()).to_be_bytes(),
                )?;
            }
        }
        Ok(())
    }

    fn get_num_accounts(&self) -> anyhow::Result<Option<u32>> {
        Ok(self
            .database
            .get(Self::TOTAL_NUM_ACCOUNTS_KEY)?
            .map(from_be_bytes))
    }

    fn update_account_balance(&self, pk: &PublicKey, balance: Option<u64>) -> anyhow::Result<()> {
        // delete account when balance is none
        if balance.is_none() {
            // delete stale data
            let b = self.get_account_balance(pk)?.unwrap_or_default();
            self.database
                .delete_cf(self.account_balance_cf(), pk.0.as_bytes())?;
            self.database
                .delete_cf(self.account_balance_sort_cf(), u64_prefix_key(b, &pk.0))?;
            return Ok(());
        }

        // update account balance when some
        let balance = balance.unwrap();
        if let Some(old) = self.get_account_balance(pk)? {
            // delete stale balance sorting data
            self.database
                .delete_cf(self.account_balance_sort_cf(), u64_prefix_key(old, &pk.0))?;
        }
        self.database.put_cf(
            self.account_balance_cf(),
            pk.0.as_bytes(),
            balance.to_be_bytes(),
        )?;

        // add: {balance}{pk} -> _
        self.database.put_cf(
            self.account_balance_sort_cf(),
            u64_prefix_key(balance, &pk.0),
            b"",
        )?;
        Ok(())
    }

    fn set_block_balance_updates(
        &self,
        state_hash: &BlockHash,
        balance_updates: &[AccountBalanceUpdate],
    ) -> anyhow::Result<()> {
        trace!("Setting block balance updates for {state_hash}");
        self.database.put_cf(
            self.account_balance_updates_cf(),
            state_hash.0.as_bytes(),
            serde_json::to_vec(balance_updates)?,
        )?;
        Ok(())
    }

    fn get_account_balance(&self, pk: &PublicKey) -> anyhow::Result<Option<u64>> {
        trace!("Getting account balance {pk}");

        Ok(self
            .database
            .get_pinned_cf(self.account_balance_cf(), pk.0.as_bytes())?
            .map(|bytes| {
                let mut be_bytes = [0; 8];
                be_bytes.copy_from_slice(&bytes[..8]);
                u64::from_be_bytes(be_bytes)
            }))
    }

    ///////////////
    // Iterators //
    ///////////////

    fn account_balance_iterator<'a>(&'a self, mode: IteratorMode) -> DBIterator<'a> {
        self.database
            .iterator_cf(self.account_balance_sort_cf(), mode)
    }
}
