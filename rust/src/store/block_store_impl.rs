use super::{column_families::ColumnFamilyHelpers, fixed_keys::FixedKeys};
use crate::{
    block::{
        precomputed::{PcbVersion, PrecomputedBlock},
        store::BlockStore,
        BlockComparison, BlockHash,
    },
    canonicity::{store::CanonicityStore, Canonicity},
    command::{internal::store::InternalCommandStore, store::UserCommandStore},
    event::{db::*, store::EventStore, IndexerEvent},
    ledger::{diff::LedgerBalanceUpdate, public_key::PublicKey},
    snark_work::store::SnarkStore,
    store::{
        account::AccountStore, block_state_hash_from_key, block_u32_prefix_from_key, from_be_bytes,
        to_be_bytes, u32_prefix_key, IndexerStore,
    },
};
use anyhow::{bail, Context};
use log::{error, trace};
use speedb::{DBIterator, Direction, IteratorMode};

impl BlockStore for IndexerStore {
    /// Add the given block at its indices and record a db event
    fn add_block(&self, block: &PrecomputedBlock) -> anyhow::Result<Option<DbEvent>> {
        trace!("Adding block {}", block.summary());

        // add block to db
        let state_hash = block.state_hash();
        if matches!(
            self.database
                .get_pinned_cf(self.blocks_cf(), state_hash.0.as_bytes()),
            Ok(Some(_))
        ) {
            trace!("Block already present {}", block.summary());
            return Ok(None);
        }
        self.database.put_cf(
            self.blocks_cf(),
            state_hash.0.as_bytes(),
            serde_json::to_vec(&block)?,
        )?;

        // add to epoch index before setting other indices
        self.set_block_epoch(&state_hash, block.epoch_count())?;

        // increment block production counts
        self.increment_block_production_count(block)?;

        // add comparison data before user commands, SNARKs, and internal commands
        self.set_block_comparison(&state_hash, &BlockComparison::from(block))?;

        // add to blockchain length index
        self.set_block_height(&state_hash, block.blockchain_length())?;

        // add to parent hash index
        self.set_block_parent_hash(&state_hash, &block.previous_state_hash())?;

        // add to genesis state hash index
        self.set_block_genesis_state_hash(&state_hash, &block.genesis_state_hash())?;

        // add block height/global slot
        self.set_block_height_global_slot_pair(
            block.blockchain_length(),
            block.global_slot_since_genesis(),
        )?;

        // add to coinbase receiver index
        self.set_coinbase_receiver(&state_hash, &block.coinbase_receiver())?;

        // add to balance update index
        self.set_block_balance_updates(
            &state_hash,
            block.coinbase_receiver(),
            LedgerBalanceUpdate::from_precomputed(block),
        )?;

        // add block height/global slot for sorting
        self.database
            .put_cf(self.blocks_height_sort_cf(), block_height_key(block), b"")?;
        self.database.put_cf(
            self.blocks_global_slot_sort_cf(),
            block_global_slot_key(block),
            b"",
        )?;

        // add block for each public key
        for pk in block.all_public_keys() {
            self.add_block_at_public_key(&pk, &state_hash)?;
        }

        // add block to height list
        self.add_block_at_height(&state_hash, block.blockchain_length())?;

        // add block to slots list
        self.add_block_at_slot(&state_hash, block.global_slot_since_genesis())?;

        // add pcb's version
        self.set_block_version(&state_hash, block.version())?;

        // add block user commands
        self.add_user_commands(block)?;

        // add block internal commands
        self.add_internal_commands(block)?;

        // add block SNARK work
        self.add_snark_work(block)?;

        // add new block db event only after all other data is added
        let db_event = DbEvent::Block(DbBlockEvent::NewBlock {
            state_hash: block.state_hash(),
            blockchain_length: block.blockchain_length(),
        });
        self.add_event(&IndexerEvent::Db(db_event.clone()))?;

        Ok(Some(db_event))
    }

    fn get_block(&self, state_hash: &BlockHash) -> anyhow::Result<Option<PrecomputedBlock>> {
        trace!("Getting block {state_hash}");
        Ok(self
            .database
            .get_pinned_cf(self.blocks_cf(), state_hash.0.as_bytes())?
            .and_then(|bytes| {
                serde_json::from_slice::<PrecomputedBlock>(&bytes)
                    .with_context(|| format!("{:?}", bytes.to_vec()))
                    .ok()
            }))
    }

    fn get_best_block(&self) -> anyhow::Result<Option<PrecomputedBlock>> {
        trace!("Getting best block");
        match self.get_best_block_hash()? {
            None => Ok(None),
            Some(state_hash) => self.get_block(&state_hash),
        }
    }

    fn get_best_block_hash(&self) -> anyhow::Result<Option<BlockHash>> {
        trace!("Getting best block hash");
        Ok(self
            .database
            .get(Self::BEST_TIP_STATE_HASH_KEY)?
            .and_then(|bytes| BlockHash::from_bytes(&bytes).ok()))
    }

    fn get_best_block_height(&self) -> anyhow::Result<Option<u32>> {
        Ok(self
            .get_best_block_hash()?
            .and_then(|state_hash| self.get_block_height(&state_hash).ok().flatten()))
    }

    fn get_best_block_global_slot(&self) -> anyhow::Result<Option<u32>> {
        Ok(self
            .get_best_block_hash()?
            .and_then(|state_hash| self.get_block_global_slot(&state_hash).ok().flatten()))
    }

    fn get_best_block_genesis_hash(&self) -> anyhow::Result<Option<BlockHash>> {
        Ok(self.get_best_block_hash()?.and_then(|state_hash| {
            self.get_block_genesis_state_hash(&state_hash)
                .ok()
                .flatten()
        }))
    }

    fn set_best_block(&self, state_hash: &BlockHash) -> anyhow::Result<()> {
        trace!("Setting best block {state_hash}");

        if let Some(old) = self.get_best_block_hash()? {
            if old == *state_hash {
                return Ok(());
            }

            let (balance_updates, coinbase_receivers) =
                self.common_ancestor_account_balance_updates(&old, state_hash)?;
            self.update_account_balances(state_hash, balance_updates, coinbase_receivers)?;
        }

        // set new best tip
        self.database
            .put(Self::BEST_TIP_STATE_HASH_KEY, state_hash.0.as_bytes())?;

        // record new best tip event
        match self.get_block_height(state_hash)? {
            Some(blockchain_length) => {
                self.add_event(&IndexerEvent::Db(DbEvent::Block(
                    DbBlockEvent::NewBestTip {
                        state_hash: state_hash.clone(),
                        blockchain_length,
                    },
                )))?;
            }
            None => error!("Block missing from store: {state_hash}"),
        }
        Ok(())
    }

    fn get_block_parent_hash(&self, state_hash: &BlockHash) -> anyhow::Result<Option<BlockHash>> {
        trace!("Getting block's parent hash {state_hash}");
        Ok(self
            .database
            .get_cf(self.block_parent_hash_cf(), state_hash.0.as_bytes())?
            .and_then(|bytes| BlockHash::from_bytes(&bytes).ok()))
    }

    fn set_block_parent_hash(
        &self,
        state_hash: &BlockHash,
        previous_state_hash: &BlockHash,
    ) -> anyhow::Result<()> {
        trace!("Setting block parent hash {state_hash}: {previous_state_hash}");
        Ok(self.database.put_cf(
            self.block_parent_hash_cf(),
            state_hash.0.as_bytes(),
            previous_state_hash.0.as_bytes(),
        )?)
    }

    fn get_block_height(&self, state_hash: &BlockHash) -> anyhow::Result<Option<u32>> {
        trace!("Getting block height {state_hash}");
        Ok(self
            .database
            .get_cf(self.block_height_cf(), state_hash.0.as_bytes())?
            .map(from_be_bytes))
    }

    fn set_block_height(
        &self,
        state_hash: &BlockHash,
        blockchain_length: u32,
    ) -> anyhow::Result<()> {
        trace!("Setting block height {state_hash}: {blockchain_length}");
        Ok(self.database.put_cf(
            self.block_height_cf(),
            state_hash.0.as_bytes(),
            to_be_bytes(blockchain_length),
        )?)
    }

    fn get_block_global_slot(&self, state_hash: &BlockHash) -> anyhow::Result<Option<u32>> {
        trace!("Getting block global slot {state_hash}");
        Ok(self
            .database
            .get_cf(self.block_global_slot_cf(), state_hash.0.as_bytes())?
            .map(from_be_bytes))
    }

    fn set_block_global_slot(
        &self,
        state_hash: &BlockHash,
        global_slot: u32,
    ) -> anyhow::Result<()> {
        trace!("Setting block global slot {state_hash}: {global_slot}");
        Ok(self.database.put_cf(
            self.block_global_slot_cf(),
            state_hash.0.as_bytes(),
            to_be_bytes(global_slot),
        )?)
    }

    fn get_coinbase_receiver(&self, state_hash: &BlockHash) -> anyhow::Result<Option<PublicKey>> {
        trace!("Getting coinbase receiver for {state_hash}");
        Ok(self
            .database
            .get_cf(self.block_coinbase_receiver_cf(), state_hash.0.as_bytes())?
            .and_then(|bytes| PublicKey::from_bytes(&bytes).ok()))
    }

    fn set_coinbase_receiver(
        &self,
        state_hash: &BlockHash,
        coinbase_receiver: &PublicKey,
    ) -> anyhow::Result<()> {
        trace!("Setting coinbase receiver: {state_hash} -> {coinbase_receiver}");
        Ok(self.database.put_cf(
            self.block_coinbase_receiver_cf(),
            state_hash.0.as_bytes(),
            coinbase_receiver.0.as_bytes(),
        )?)
    }

    fn get_num_blocks_at_height(&self, blockchain_length: u32) -> anyhow::Result<u32> {
        trace!("Getting number of blocks at height {blockchain_length}");
        Ok(self
            .database
            .get_cf(self.blocks_at_height_cf(), to_be_bytes(blockchain_length))?
            .map_or(0, from_be_bytes))
    }

    fn add_block_at_height(
        &self,
        state_hash: &BlockHash,
        blockchain_length: u32,
    ) -> anyhow::Result<()> {
        trace!("Adding block {state_hash} at height {blockchain_length}");

        // increment num blocks at height
        let num_blocks_at_height = self.get_num_blocks_at_height(blockchain_length)?;
        self.database.put_cf(
            self.blocks_at_height_cf(),
            to_be_bytes(blockchain_length),
            to_be_bytes(num_blocks_at_height + 1),
        )?;

        // add the new key-value pair
        Ok(self.database.put_cf(
            self.blocks_at_height_cf(),
            format!("{blockchain_length}-{num_blocks_at_height}"),
            state_hash.0.as_bytes(),
        )?)
    }

    fn get_blocks_at_height(
        &self,
        blockchain_length: u32,
    ) -> anyhow::Result<Vec<PrecomputedBlock>> {
        let num_blocks_at_height = self.get_num_blocks_at_height(blockchain_length)?;
        let mut blocks = vec![];

        for n in 0..num_blocks_at_height {
            match self.database.get_cf(
                self.blocks_at_height_cf(),
                format!("{blockchain_length}-{n}"),
            )? {
                None => break,
                Some(bytes) => {
                    let state_hash = BlockHash::from_bytes(&bytes)?;
                    if let Some(block) = self.get_block(&state_hash)? {
                        blocks.push(block);
                    }
                }
            }
        }

        blocks.sort_by(|a, b| {
            use std::cmp::Ordering;
            let a_canonicity = self.get_block_canonicity(&a.state_hash()).ok().flatten();
            let b_canonicity = self.get_block_canonicity(&b.state_hash()).ok().flatten();
            match (a_canonicity, b_canonicity) {
                (Some(Canonicity::Canonical), _) => Ordering::Less,
                (_, Some(Canonicity::Canonical)) => Ordering::Greater,
                _ => a.cmp(b),
            }
        });
        Ok(blocks)
    }

    fn get_num_blocks_at_slot(&self, slot: u32) -> anyhow::Result<u32> {
        trace!("Getting number of blocks at slot {slot}");
        Ok(self
            .database
            .get_cf(self.blocks_at_global_slot_cf(), to_be_bytes(slot))?
            .map_or(0, from_be_bytes))
    }

    fn add_block_at_slot(&self, state_hash: &BlockHash, slot: u32) -> anyhow::Result<()> {
        trace!("Adding block {state_hash} at slot {slot}");

        // increment num blocks at slot
        let num_blocks_at_slot = self.get_num_blocks_at_slot(slot)?;
        self.database.put_cf(
            self.blocks_at_global_slot_cf(),
            to_be_bytes(slot),
            to_be_bytes(num_blocks_at_slot + 1),
        )?;

        // add the new key-value pair
        Ok(self.database.put_cf(
            self.blocks_at_global_slot_cf(),
            format!("{slot}-{num_blocks_at_slot}"),
            state_hash.0.as_bytes(),
        )?)
    }

    fn get_blocks_at_slot(&self, slot: u32) -> anyhow::Result<Vec<PrecomputedBlock>> {
        trace!("Getting blocks at slot {slot}");

        let num_blocks_at_slot = self.get_num_blocks_at_slot(slot)?;
        let mut blocks = vec![];

        for n in 0..num_blocks_at_slot {
            match self
                .database
                .get_cf(self.blocks_at_global_slot_cf(), format!("{slot}-{n}"))?
            {
                None => break,
                Some(bytes) => {
                    let state_hash = BlockHash::from_bytes(&bytes)?;
                    if let Some(block) = self.get_block(&state_hash)? {
                        blocks.push(block);
                    }
                }
            }
        }

        blocks.sort_by(|a, b| {
            use std::cmp::Ordering;
            let a_canonicity = self.get_block_canonicity(&a.state_hash()).ok().flatten();
            let b_canonicity = self.get_block_canonicity(&b.state_hash()).ok().flatten();
            match (a_canonicity, b_canonicity) {
                (Some(Canonicity::Canonical), _) => Ordering::Less,
                (_, Some(Canonicity::Canonical)) => Ordering::Greater,
                _ => a.cmp(b),
            }
        });
        Ok(blocks)
    }

    fn get_num_blocks_at_public_key(&self, pk: &PublicKey) -> anyhow::Result<u32> {
        trace!("Getting number of blocks at public key {pk}");
        Ok(
            match self
                .database
                .get_pinned_cf(self.blocks_cf(), pk.to_string().as_bytes())?
            {
                None => 0,
                Some(bytes) => String::from_utf8(bytes.to_vec())?.parse()?,
            },
        )
    }

    fn add_block_at_public_key(
        &self,
        pk: &PublicKey,
        state_hash: &BlockHash,
    ) -> anyhow::Result<()> {
        trace!("Adding block {state_hash} at public key {pk}");

        // increment num blocks at public key
        let num_blocks_at_pk = self.get_num_blocks_at_public_key(pk)?;
        self.database.put_cf(
            self.blocks_cf(),
            pk.to_string().as_bytes(),
            (num_blocks_at_pk + 1).to_string().as_bytes(),
        )?;

        // add the new key-value pair
        let key = format!("{pk}-{num_blocks_at_pk}");
        Ok(self.database.put_cf(
            self.blocks_cf(),
            key.as_bytes(),
            state_hash.to_string().as_bytes(),
        )?)
    }

    fn get_blocks_at_public_key(&self, pk: &PublicKey) -> anyhow::Result<Vec<PrecomputedBlock>> {
        trace!("Getting blocks at public key {pk}");

        let num_blocks_at_pk = self.get_num_blocks_at_public_key(pk)?;
        let mut blocks = vec![];

        for n in 0..num_blocks_at_pk {
            let key = format!("{pk}-{n}");
            match self
                .database
                .get_pinned_cf(self.blocks_cf(), key.as_bytes())?
            {
                None => break,
                Some(bytes) => {
                    let state_hash = BlockHash::from_bytes(&bytes)?;
                    if let Some(block) = self.get_block(&state_hash)? {
                        blocks.push(block);
                    }
                }
            }
        }

        blocks.sort();
        Ok(blocks)
    }

    fn get_block_children(&self, state_hash: &BlockHash) -> anyhow::Result<Vec<PrecomputedBlock>> {
        trace!("Getting children of block {}", state_hash);

        if let Some(height) = self.get_block(state_hash)?.map(|b| b.blockchain_length()) {
            let blocks_at_next_height = self.get_blocks_at_height(height + 1)?;
            let mut children: Vec<PrecomputedBlock> = blocks_at_next_height
                .into_iter()
                .filter(|b| b.previous_state_hash() == *state_hash)
                .collect();
            children.sort();
            return Ok(children);
        }
        bail!("Block missing from store {}", state_hash)
    }

    fn get_block_version(&self, state_hash: &BlockHash) -> anyhow::Result<Option<PcbVersion>> {
        trace!("Getting block {} version", state_hash.0);
        let key = state_hash.0.as_bytes();
        Ok(self
            .database
            .get_pinned_cf(self.block_version_cf(), key)?
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
    }

    fn set_block_version(&self, state_hash: &BlockHash, version: PcbVersion) -> anyhow::Result<()> {
        trace!("Setting block {} version to {}", state_hash.0, version);
        Ok(self.database.put_cf(
            self.block_version_cf(),
            state_hash.0.as_bytes(),
            serde_json::to_vec(&version)?,
        )?)
    }

    fn set_block_height_global_slot_pair(
        &self,
        blockchain_length: u32,
        global_slot: u32,
    ) -> anyhow::Result<()> {
        trace!("Setting block height {blockchain_length} <-> slot {global_slot}");

        // add slot to height's "slot collection"
        let mut heights = self
            .get_block_heights_from_global_slot(global_slot)?
            .unwrap_or_default();
        if !heights.contains(&blockchain_length) {
            heights.push(blockchain_length);
            heights.sort();
            self.database.put_cf(
                self.block_global_slot_to_heights_cf(),
                to_be_bytes(global_slot),
                serde_json::to_vec(&heights)?,
            )?;
        }

        // add height to slot's "height collection"
        let mut slots = self
            .get_global_slots_from_height(blockchain_length)?
            .unwrap_or_default();
        if !slots.contains(&global_slot) {
            slots.push(global_slot);
            slots.sort();
            self.database.put_cf(
                self.block_height_to_global_slots_cf(),
                to_be_bytes(blockchain_length),
                serde_json::to_vec(&slots)?,
            )?;
        }
        Ok(())
    }

    fn get_global_slots_from_height(
        &self,
        blockchain_length: u32,
    ) -> anyhow::Result<Option<Vec<u32>>> {
        trace!("Getting global slot for height {}", blockchain_length);
        Ok(self
            .database
            .get_pinned_cf(
                self.block_global_slot_to_heights_cf(),
                to_be_bytes(blockchain_length),
            )?
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
    }

    fn get_block_heights_from_global_slot(
        &self,
        global_slot: u32,
    ) -> anyhow::Result<Option<Vec<u32>>> {
        trace!("Getting height for global slot {global_slot}");
        Ok(self
            .database
            .get_pinned_cf(
                self.block_height_to_global_slots_cf(),
                to_be_bytes(global_slot),
            )?
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
    }

    fn get_current_epoch(&self) -> anyhow::Result<u32> {
        Ok(self
            .get_best_block_hash()?
            .and_then(|state_hash| self.get_block_epoch(&state_hash).ok().flatten())
            .unwrap_or_default())
    }

    fn set_block_epoch(&self, state_hash: &BlockHash, epoch: u32) -> anyhow::Result<()> {
        trace!("Setting block epoch {epoch}: {state_hash}");
        Ok(self.database.put_cf(
            self.block_epoch_cf(),
            state_hash.0.as_bytes(),
            to_be_bytes(epoch),
        )?)
    }

    fn get_block_epoch(&self, state_hash: &BlockHash) -> anyhow::Result<Option<u32>> {
        trace!("Getting block epoch {state_hash}");
        Ok(self
            .database
            .get_cf(self.block_epoch_cf(), state_hash.0.as_bytes())?
            .map(from_be_bytes))
    }

    fn set_block_genesis_state_hash(
        &self,
        state_hash: &BlockHash,
        genesis_state_hash: &BlockHash,
    ) -> anyhow::Result<()> {
        trace!("Setting block genesis state hash {state_hash}: {genesis_state_hash}");
        Ok(self.database.put_cf(
            self.block_genesis_state_hash_cf(),
            state_hash.0.as_bytes(),
            genesis_state_hash.0.as_bytes(),
        )?)
    }

    fn get_block_genesis_state_hash(
        &self,
        state_hash: &BlockHash,
    ) -> anyhow::Result<Option<BlockHash>> {
        trace!("Getting block genesis state hash {state_hash}");
        Ok(self
            .database
            .get_cf(self.block_genesis_state_hash_cf(), state_hash.0.as_bytes())?
            .and_then(|bytes| BlockHash::from_bytes(&bytes).ok()))
    }

    ///////////////
    // Iterators //
    ///////////////

    fn blocks_height_iterator<'a>(&'a self, mode: IteratorMode) -> DBIterator<'a> {
        self.database
            .iterator_cf(self.blocks_height_sort_cf(), mode)
    }

    fn blocks_global_slot_iterator<'a>(&'a self, mode: IteratorMode) -> DBIterator<'a> {
        self.database
            .iterator_cf(self.blocks_global_slot_sort_cf(), mode)
    }

    //////////////////
    // Block counts //
    //////////////////

    fn increment_block_production_count(&self, block: &PrecomputedBlock) -> anyhow::Result<()> {
        trace!("Incrementing block production count {}", block.summary());

        let creator = block.block_creator();
        let epoch = block.epoch_count();

        // increment pk epoch count
        let acc = self.get_block_production_pk_epoch_count(&creator, Some(epoch))?;
        self.database.put_cf(
            self.block_production_pk_epoch_cf(),
            u32_prefix_key(epoch, &creator.0),
            to_be_bytes(acc + 1),
        )?;

        // increment pk total count
        let acc = self.get_block_production_pk_total_count(&creator)?;
        self.database.put_cf(
            self.block_production_pk_total_cf(),
            creator.to_bytes(),
            to_be_bytes(acc + 1),
        )?;

        // increment epoch count
        let acc = self.get_block_production_epoch_count(Some(epoch))?;
        self.database.put_cf(
            self.block_production_epoch_cf(),
            to_be_bytes(epoch),
            to_be_bytes(acc + 1),
        )?;

        // increment total count
        let acc = self.get_block_production_total_count()?;
        self.database
            .put(Self::TOTAL_NUM_BLOCKS_KEY, to_be_bytes(acc + 1))?;

        Ok(())
    }

    fn get_block_production_pk_epoch_count(
        &self,
        pk: &PublicKey,
        epoch: Option<u32>,
    ) -> anyhow::Result<u32> {
        let epoch = epoch.unwrap_or(self.get_current_epoch()?);
        trace!("Getting pk epoch {epoch} block production count {pk}");
        Ok(self
            .database
            .get_cf(
                self.block_production_pk_epoch_cf(),
                u32_prefix_key(epoch, &pk.0),
            )?
            .map_or(0, from_be_bytes))
    }

    fn get_block_production_pk_total_count(&self, pk: &PublicKey) -> anyhow::Result<u32> {
        trace!("Getting pk total block production count {pk}");
        Ok(self
            .database
            .get_cf(self.block_production_pk_total_cf(), pk.clone().to_bytes())?
            .map_or(0, from_be_bytes))
    }

    fn get_block_production_epoch_count(&self, epoch: Option<u32>) -> anyhow::Result<u32> {
        let epoch = epoch.unwrap_or(self.get_current_epoch()?);
        trace!("Getting epoch block production count {epoch}");
        Ok(self
            .database
            .get_cf(self.block_production_epoch_cf(), to_be_bytes(epoch))?
            .map_or(0, from_be_bytes))
    }

    fn get_block_production_total_count(&self) -> anyhow::Result<u32> {
        trace!("Getting total block production count");
        Ok(self
            .database
            .get(Self::TOTAL_NUM_BLOCKS_KEY)?
            .map_or(0, from_be_bytes))
    }

    fn set_block_comparison(
        &self,
        state_hash: &BlockHash,
        comparison: &BlockComparison,
    ) -> anyhow::Result<()> {
        trace!("Setting block comparison {state_hash}");
        Ok(self.database.put_cf(
            self.block_comparison_cf(),
            state_hash.0.as_bytes(),
            serde_json::to_vec(comparison)?,
        )?)
    }

    fn get_block_comparison(
        &self,
        state_hash: &BlockHash,
    ) -> anyhow::Result<Option<BlockComparison>> {
        trace!("Getting block comparison {state_hash}");
        Ok(self
            .database
            .get_pinned_cf(self.block_comparison_cf(), state_hash.0.as_bytes())?
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
    }

    fn block_cmp(
        &self,
        block: &BlockHash,
        other: &BlockHash,
    ) -> anyhow::Result<Option<std::cmp::Ordering>> {
        // get stored block comparisons
        let res1 = self
            .database
            .get_cf(self.block_comparison_cf(), block.0.as_bytes());
        let res2 = self
            .database
            .get_cf(self.block_comparison_cf(), other.0.as_bytes());

        // compare stored block comparisons
        if let (Ok(Some(bytes1)), Ok(Some(bytes2))) = (res1, res2) {
            let bc1: BlockComparison = serde_json::from_slice(&bytes1)?;
            let bc2: BlockComparison = serde_json::from_slice(&bytes2)?;
            return Ok(Some(bc1.cmp(&bc2)));
        }
        Ok(None)
    }

    fn dump_blocks_via_height(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use std::{fs::File, io::Write};
        trace!("Dumping blocks via height to {}", path.display());
        let mut file = File::create(path)?;

        for (key, _) in self
            .blocks_height_iterator(speedb::IteratorMode::Start)
            .flatten()
        {
            let state_hash = block_state_hash_from_key(&key)?;
            let block_height = block_u32_prefix_from_key(&key)?;
            let global_slot = self
                .get_block_global_slot(&state_hash)?
                .expect("global slot");

            writeln!(
                file,
                "height: {block_height}\nslot:   {global_slot}\nstate:  {state_hash}"
            )?;
        }
        Ok(())
    }

    fn blocks_via_height(&self, mode: IteratorMode) -> anyhow::Result<Vec<PrecomputedBlock>> {
        let mut blocks = vec![];
        trace!("Getting blocks via height (mode: {})", display_mode(mode));
        for (key, _) in self.blocks_height_iterator(mode).flatten() {
            let state_hash = block_state_hash_from_key(&key)?;
            blocks.push(self.get_block(&state_hash)?.expect("PCB"));
        }
        Ok(blocks)
    }

    fn dump_blocks_via_global_slot(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use std::{fs::File, io::Write};
        trace!("Dumping blocks via global slot to {}", path.display());
        let mut file = File::create(path)?;

        for (key, _) in self
            .blocks_global_slot_iterator(speedb::IteratorMode::Start)
            .flatten()
        {
            let state_hash = block_state_hash_from_key(&key)?;
            let block_height = block_u32_prefix_from_key(&key)?;
            let global_slot = self
                .get_block_global_slot(&state_hash)?
                .expect("global slot");

            writeln!(
                file,
                "height: {block_height}\nslot:   {global_slot}\nstate:  {state_hash}"
            )?;
        }
        Ok(())
    }

    fn blocks_via_global_slot(&self, mode: IteratorMode) -> anyhow::Result<Vec<PrecomputedBlock>> {
        let mut blocks = vec![];
        trace!(
            "Getting blocks via global slot (mode: {})",
            display_mode(mode)
        );
        for (key, _) in self.blocks_global_slot_iterator(mode).flatten() {
            let state_hash = block_state_hash_from_key(&key)?;
            blocks.push(self.get_block(&state_hash)?.expect("PCB"));
        }
        Ok(blocks)
    }
}

/// `{block height BE}{state hash}`
fn block_height_key(block: &PrecomputedBlock) -> Vec<u8> {
    let mut key = to_be_bytes(block.blockchain_length());
    key.append(&mut block.state_hash().to_bytes());
    key
}

/// `{global slot BE}{state hash}`
fn block_global_slot_key(block: &PrecomputedBlock) -> Vec<u8> {
    let mut key = to_be_bytes(block.global_slot_since_genesis());
    key.append(&mut block.state_hash().to_bytes());
    key
}

fn display_mode(mode: IteratorMode) -> String {
    match mode {
        IteratorMode::End => "End".to_string(),
        IteratorMode::Start => "Start".to_string(),
        IteratorMode::From(start, direction) => {
            format!("{} from {start:?}", display_direction(direction))
        }
    }
}

fn display_direction(direction: Direction) -> String {
    match direction {
        Direction::Forward => "Forward".to_string(),
        Direction::Reverse => "Reverse".to_string(),
    }
}
