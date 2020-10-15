use crate::block::{Block, Content, Data, Proof};
use crate::farmer::Solution;
use crate::timer::EpochTracker;
use crate::transaction::{
    AccountAddress, AccountState, CoinbaseTx, SimpleCreditTx, Transaction, TxId,
};
use crate::{
    crypto, sloth, ContentId, ProofId, Tag, BLOCK_REWARD, CHALLENGE_LOOKBACK_EPOCHS,
    CONFIRMATION_DEPTH, EXPECTED_TIMESLOTS_PER_EON, GENESIS_OFFSET, INITIAL_SOLUTION_RANGE,
    MAX_EARLY_TIMESLOTS, MAX_LATE_TIMESLOTS, PRIME_SIZE_BITS, PROPOSER_BLOCKS_PER_EON,
    SOLUTION_RANGE_UPDATE_DELAY_IN_TIMESLOTS, TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION,
    TX_BLOCKS_PER_PROPOSER_BLOCK,
};

use crate::metablocks::{MetaBlock, MetaBlocks};
use async_std::task::JoinHandle;
use log::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::TryInto;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/* TESTING
 * Piece count is always 256 for testing for the merkle tree
 * Plot size is configurable, but must be a multiple of 256
 * For each challenge, solver will check every 256th piece starting at index and return the top N
 * We want to start with 256 x 256 pieces
 * This mean for each challenge the expected quality should be below 2^32 / 2^8 -> 2^24
 *
*/

// TODO: can we sync blocks by epoch

// sync process
// each proposer block and state block is stored in blocks by timeslot
// request at each timeslot and return all blocks for that slot
// bundle any transactions with transaction blocks

// add a notion of tx blocks

pub type BlockHeight = u64;
pub type Timeslot = u64;

#[derive(Debug, Clone)]
pub struct Head {
    block_height: u64,
    content_id: ContentId,
}

pub struct SolutionRangeUpdate {
    next_timeslot: u64,
    block_height: u64,
    solution_range: u64,
}

impl SolutionRangeUpdate {
    pub fn new(next_timeslot: u64, block_height: u64, solution_range: u64) -> Self {
        SolutionRangeUpdate {
            next_timeslot,
            block_height,
            solution_range,
        }
    }
}

// block: cached || staged
// cached due to: received before sync or blocks found close together

// on receipt of new block via gossip
// if the parent is in metablocks -> stage
// else -> cache into cached_blocks_by_pending_parent: <ContentId, Vec<Block>>
// on receipt of parent -> stage cached block

// what to do when we are syncing blocks?
// cache all incoming gossip
// request blocks from peer by timeslot, validate and stage
// if block is cached, remove from cache
// once we arrive at the current timeslot, apply all cached gossip

pub struct Ledger {
    /// the current confirmed credit balance of all subspace accounts
    pub balances: HashMap<AccountAddress, AccountState>,
    /// storage container for blocks with metadata
    pub metablocks: MetaBlocks,
    /// proof_ids for the last N blocks, to prevent duplicate gossip and content spamming
    pub recent_proof_ids: HashSet<ProofId>,
    /// record that allows for syncing the ledger by timeslot
    pub proof_ids_by_timeslot: BTreeMap<Timeslot, Vec<ProofId>>,
    /// container for proposer blocks received who have an unknown parent
    pub cached_proposer_blocks_by_parent_content_id: HashMap<ContentId, Vec<Block>>,
    /// container for tx blocks received via gossip during sync
    pub cached_tx_blocks_by_content_id: HashMap<ContentId, Block>,
    /// temporary container for blocks seen before their timeslot has arrived
    pub early_blocks_by_timeslot: BTreeMap<Timeslot, Vec<Block>>,
    /// fork tracker for pending blocks, used to find the current head of longest chain
    pub heads: Vec<Head>,
    /// Proof ids of all k-deep (confirmed) blocks
    // TODO: may be able to remove this
    confirmed_blocks: HashSet<ProofId>,
    last_eon_close_timeslot: u64,
    // data for next solution range
    pub solution_range_update: SolutionRangeUpdate,
    /// solution range being used at this time
    pub current_solution_range: u64,
    /// solution range tracker for each eon
    pub solution_ranges_by_eon: HashMap<u64, u64>,
    /// container for all txs
    pub txs: HashMap<TxId, Transaction>,
    /// tracker for txs that have not yet been referenced in a tx block
    pub unclaimed_tx_ids: HashSet<TxId>,
    /// tracker for txs that have been referenced in a tx block but have not been seen yet
    pub unknown_tx_ids: HashSet<TxId>,
    /// tracker for all txs that have been applied to the ledger
    pub applied_tx_ids: HashSet<TxId>,
    /// tracker for tx blocks that have not yet been referenced in a proposer block
    pub unclaimed_tx_block_ids: HashSet<ContentId>,
    /// tracker for tx blocks that have been referenced in a proposer block buy have not been seen yet
    pub unknown_tx_block_ids: HashSet<ContentId>,
    /// tracker for tx blocks that have been applied to the ledger
    pub applied_tx_block_ids: HashSet<ContentId>,
    pub epoch_tracker: EpochTracker,
    pub timer_is_running: bool,
    pub quality: u32,
    pub keys: ed25519_dalek::Keypair,
    pub sloth: sloth::Sloth,
    pub genesis_timestamp: u64,
    pub genesis_piece_hash: [u8; 32],
    pub merkle_root: Vec<u8>,
    pub merkle_proofs: Vec<Vec<u8>>,
    pub tx_payload: Vec<u8>,
    pub current_timeslot: u64,
    timer_handle: Option<JoinHandle<()>>,
}

impl Drop for Ledger {
    fn drop(&mut self) {
        let timer_handle: JoinHandle<()> = self.timer_handle.take().unwrap();
        async_std::task::spawn(async move {
            timer_handle.cancel().await;
        });
    }
}

impl Ledger {
    pub fn new(
        merkle_root: Vec<u8>,
        genesis_piece_hash: [u8; 32],
        keys: ed25519_dalek::Keypair,
        tx_payload: Vec<u8>,
        merkle_proofs: Vec<Vec<u8>>,
        epoch_tracker: EpochTracker,
    ) -> Ledger {
        // init sloth
        let prime_size = PRIME_SIZE_BITS;
        let sloth = sloth::Sloth::init(prime_size);

        let timer_handle = async_std::task::spawn(async {
            // TODO: listen on the channel
            // listen for the next timeslot
            // increment the timeslot count
            // stage early blocks for that timeslot
        });

        // TODO: all of these data structures need to be periodically truncated
        let mut ledger = Ledger {
            balances: HashMap::new(),
            metablocks: MetaBlocks::new(),
            recent_proof_ids: HashSet::new(),
            proof_ids_by_timeslot: BTreeMap::new(),
            cached_proposer_blocks_by_parent_content_id: HashMap::new(),
            cached_tx_blocks_by_content_id: HashMap::new(),
            early_blocks_by_timeslot: BTreeMap::new(),
            heads: Vec::new(),
            confirmed_blocks: HashSet::new(),
            solution_range_update: SolutionRangeUpdate::new(0, 0, 0),
            current_solution_range: INITIAL_SOLUTION_RANGE,
            solution_ranges_by_eon: HashMap::new(),
            txs: HashMap::new(),
            unclaimed_tx_ids: HashSet::new(),
            unknown_tx_ids: HashSet::new(),
            applied_tx_ids: HashSet::new(),
            unclaimed_tx_block_ids: HashSet::new(),
            unknown_tx_block_ids: HashSet::new(),
            applied_tx_block_ids: HashSet::new(),
            genesis_timestamp: 0,
            timer_is_running: false,
            quality: 0,
            epoch_tracker,
            merkle_root,
            genesis_piece_hash,
            sloth,
            keys,
            tx_payload,
            merkle_proofs,
            current_timeslot: 0,
            timer_handle: Some(timer_handle),
            last_eon_close_timeslot: GENESIS_OFFSET,
        };

        ledger
            .solution_ranges_by_eon
            .insert(0, INITIAL_SOLUTION_RANGE);

        ledger
    }

    /// Update the timeslot, then validates and stages all early blocks that have arrived
    pub async fn next_timeslot(&mut self) {
        self.current_timeslot += 1;
        info!("Ledger has arrived at timeslot {}", self.current_timeslot);

        // apply solution range changes with delay
        if self.current_timeslot == self.solution_range_update.next_timeslot {
            self.current_solution_range = self.solution_range_update.solution_range;

            // track the current eon_index
            let eon_index = (self.solution_range_update.block_height - GENESIS_OFFSET)
                / PROPOSER_BLOCKS_PER_EON;
            self.solution_ranges_by_eon
                .insert(eon_index, self.current_solution_range);
        }

        // apply all early blocks
        if self
            .early_blocks_by_timeslot
            .contains_key(&self.current_timeslot)
        {
            for block in self
                .early_blocks_by_timeslot
                .get(&self.current_timeslot)
                .unwrap()
                .clone()
            {
                debug!("Timeslot has arrived for early block, validating and staging");

                if self.validate_block_that_has_arrived(&block).await {
                    // TODO: have to make sure we don't reference the block (just check for last timeslot in create block)
                    if block.content.parent_id.is_some() {
                        self.stage_proposer_block(&block).await;
                    } else {
                        self.stage_tx_block(&block).await;
                    }
                }
            }

            self.early_blocks_by_timeslot.remove(&self.current_timeslot);
        }
    }

    /// Returns all (valid) blocks seen for a given timeslot
    pub fn get_blocks_by_timeslot(&self, timeslot: u64) -> Vec<Block> {
        self.proof_ids_by_timeslot
            .get(&timeslot)
            .map(|proof_ids| {
                proof_ids
                    .iter()
                    .map(|proof_id| self.metablocks.get_metablock_from_proof_id(proof_id).block)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns all txs ref'd by a set of tx blocks for a given timeslot
    pub fn get_txs_for_sync(&self, blocks: &Vec<Block>) -> Vec<SimpleCreditTx> {
        let mut txs: HashMap<ContentId, SimpleCreditTx> = HashMap::new();

        blocks
            .iter()
            .filter(|block| block.content.parent_id.is_none())
            .map(|block| {
                self.metablocks
                    .get_metablock_from_content_id(&block.content.get_id())
            })
            .for_each(|tx_metablock| {
                tx_metablock
                    .block
                    .content
                    .refs
                    .iter()
                    .skip(1)
                    .for_each(|tx_id| {
                        let tx = self.txs.get(tx_id).expect("Should have tx").clone();
                        match tx {
                            Transaction::Credit(tx) => {
                                txs.insert(*tx_id, tx);
                            }
                            _ => {
                                panic!("Coinbase tx should not be ref'd in a tx block!");
                            }
                        }
                    })
            });

        txs.into_values().collect()
    }

    /// Add a block to a given timeslot, to allow other nodes to sync the ledger
    fn add_block_to_timeslot(&mut self, timeslot: Timeslot, proof_id: ProofId) {
        self.proof_ids_by_timeslot
            .entry(timeslot)
            .and_modify(|blocks| blocks.push(proof_id))
            .or_insert(vec![proof_id]);
    }

    /// returns the tip of the longest chain as seen by this node
    fn get_head(&self) -> ContentId {
        self.heads[0].content_id
    }

    /// updates an existing branch, setting to head if longest, or creates a new branch
    fn update_heads(
        &mut self,
        parent_content_id: ContentId,
        content_id: ContentId,
        block_height: u64,
    ) {
        for (index, head) in self.heads.iter_mut().enumerate() {
            if head.content_id == parent_content_id {
                // updated existing head
                head.block_height += 1;
                head.content_id = content_id;

                // check if existing branch has overtaken the current head
                if index != 0 && head.block_height > self.heads[0].block_height {
                    self.heads.swap(0, index);
                }
                return;
            }
        }

        // else create a new branch -- cannot be longest head (unless first head)
        self.heads.push(Head {
            content_id,
            block_height,
        });

        debug!(
            "Added a new head at height: {} w/content_id: {}!",
            block_height,
            hex::encode(&content_id[0..8])
        );
    }

    /// removes a branch that is equal to the current confirmed ledger
    fn prune_branch(&mut self, content_id: ContentId) {
        let mut remove_index = 0;
        for (index, head) in self.heads.iter().enumerate() {
            if head.content_id == content_id {
                remove_index = index;
            }
        }

        if remove_index == 0 {
            panic!("Cannot prune head of the longest chain!");
        }

        self.heads.remove(remove_index);
    }

    /// Start a new chain from genesis as a gateway node
    // TODO: this should solve from some genesis state block
    pub async fn init_from_genesis(&mut self) -> u64 {
        self.genesis_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;

        let mut timestamp = self.genesis_timestamp as u64;
        let mut parent_id: ContentId = [0u8; 32];

        for _ in 0..CHALLENGE_LOOKBACK_EPOCHS {
            let current_epoch_index = self.epoch_tracker.advance_epoch().await;
            info!(
                "Advanced to epoch {} during genesis init",
                current_epoch_index
            );

            for current_timeslot in (0..TIMESLOTS_PER_EPOCH)
                .map(|timeslot_index| timeslot_index + current_epoch_index * TIMESLOTS_PER_EPOCH)
            {
                let proof = Proof {
                    randomness: self.genesis_piece_hash,
                    epoch: current_epoch_index,
                    timeslot: current_timeslot,
                    public_key: self.keys.public.to_bytes(),
                    tag: Tag::default(),
                    // TODO: Fix this
                    nonce: u64::from_le_bytes(
                        crypto::create_hmac(&[], b"subspace")[0..8]
                            .try_into()
                            .unwrap(),
                    ),
                    piece_index: 0,
                    solution_range: self.current_solution_range,
                };

                let proof_id = proof.get_id();
                let coinbase_tx = CoinbaseTx::new(BLOCK_REWARD, self.keys.public, proof_id);

                let mut content = Content {
                    parent_id: Some(parent_id),
                    proof_id,
                    proof_signature: self.keys.sign(&proof_id).to_bytes().to_vec(),
                    timestamp,
                    refs: vec![coinbase_tx.get_id()],
                    signature: Vec::new(),
                };

                content.signature = self.keys.sign(&content.get_id()).to_bytes().to_vec();

                let data = Data {
                    encoding: Vec::new(),
                    merkle_proof: Vec::new(),
                };

                let block = Block {
                    proof,
                    coinbase_tx,
                    content,
                    data: Some(data),
                };

                // prepare the block for application to the ledger
                self.stage_proposer_block(&block).await;

                parent_id = block.content.get_id();

                debug!(
                    "Applied a genesis block to ledger with content id {}",
                    hex::encode(&parent_id[0..8])
                );
                let time_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_millis();

                timestamp += TIMESLOT_DURATION;

                //TODO: this should wait for the correct time to arrive rather than waiting for a fixed amount of time
                async_std::task::sleep(Duration::from_millis(timestamp - time_now as u64)).await;
            }
        }

        self.genesis_timestamp
    }

    /// create a new block locally from a valid farming solution
    pub async fn create_and_apply_local_block(
        &mut self,
        solution: Solution,
        sibling_content_ids: Vec<ContentId>,
    ) -> Block {
        let proof = Proof {
            randomness: solution.randomness,
            epoch: solution.epoch_index,
            timeslot: solution.timeslot,
            public_key: self.keys.public.to_bytes(),
            tag: solution.tag,
            // TODO: Fix this
            nonce: u64::from_le_bytes(
                crypto::create_hmac(&solution.encoding, b"subspace")[0..8]
                    .try_into()
                    .unwrap(),
            ),
            piece_index: solution.piece_index,
            solution_range: solution.solution_range,
        };
        let data = Data {
            encoding: solution.encoding.to_vec(),
            merkle_proof: crypto::get_merkle_proof(solution.proof_index, &self.merkle_proofs),
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;

        // create the coinbase tx
        let proof_id = proof.get_id();
        let coinbase_tx = CoinbaseTx::new(BLOCK_REWARD, self.keys.public, proof_id);
        let mut refs = vec![coinbase_tx.get_id()];

        // sortition between proposer blocks and tx blocks

        let target = u64::from_be_bytes(solution.target);
        let tag = u64::from_be_bytes(solution.tag);

        let proposer_block_solution_range =
            solution.solution_range / (TX_BLOCKS_PER_PROPOSER_BLOCK + 1);
        let (lower, is_lower_overflowed) =
            target.overflowing_sub(proposer_block_solution_range / 2);
        let (upper, is_upper_overflowed) =
            target.overflowing_add(proposer_block_solution_range / 2);
        let within_proposer_block_solution_range = if is_lower_overflowed || is_upper_overflowed {
            upper <= tag || tag <= lower
        } else {
            lower <= tag && tag <= upper
        };

        let parent_id: Option<ContentId> = if within_proposer_block_solution_range {
            // proposer block

            // ref the tip of the longest chain
            let mut longest_content_id = self.get_head();

            debug!("Tracking {} heads", { self.heads.len() });
            for head in self.heads.iter() {
                debug!(
                    "Block height: {} and content_id: {}",
                    head.block_height,
                    hex::encode(&head.content_id[0..8])
                );
            }

            // edge case where this node farms multiple proposer blocks in the same timeslot
            if sibling_content_ids
                .iter()
                .any(|content_id| content_id == &longest_content_id)
            {
                // the block is referencing a sibling, get its parent instead
                let sibling_metablock = self
                    .metablocks
                    .get_metablock_from_content_id(&longest_content_id);

                longest_content_id = sibling_metablock
                    .block
                    .content
                    .parent_id
                    .expect("Sibling will be a proposer block");

                debug!("Found two proposer blocks at the same height with shared parent");
            }

            debug!(
                "Parent content id for locally created block is: {}",
                hex::encode(&longest_content_id[0..8])
            );

            // ref all unseen tx blocks
            let mut pending_tx_block_ids: Vec<TxId> = self.unclaimed_tx_block_ids.drain().collect();
            pending_tx_block_ids.sort();
            for tx_block_id in pending_tx_block_ids.into_iter() {
                refs.push(tx_block_id);
            }

            Some(longest_content_id)
        } else {
            // tx block

            // ref all unseen txs in the mempool, sorted by hash
            let mut pending_tx_ids: Vec<TxId> = self.unclaimed_tx_ids.drain().collect();
            pending_tx_ids.sort();
            for tx_id in pending_tx_ids.into_iter() {
                refs.push(tx_id);
            }

            None
        };

        let mut content = Content {
            parent_id,
            proof_id,
            proof_signature: self.keys.sign(&proof.get_id()).to_bytes().to_vec(),
            timestamp,
            refs,
            signature: Vec::new(),
        };

        content.signature = self.keys.sign(&content.get_id()).to_bytes().to_vec();

        if content.parent_id.is_none() {
            self.unclaimed_tx_block_ids.insert(content.get_id());
        }

        let block = Block {
            proof,
            coinbase_tx,
            content,
            data: Some(data),
        };

        let is_valid = self.validate_block(&block).await;
        assert!(is_valid, "Local block must always be valid");

        block
    }

    /// Validates that a block is internally consistent
    async fn validate_block(&self, block: &Block) -> bool {
        // TODO: how to validate the genesis block, which has no lookback?

        // get correct randomness for this block
        let epoch = self
            .epoch_tracker
            .get_lookback_epoch(block.proof.epoch)
            .await;

        if !epoch.is_closed {
            // TODO: ensure this cannot be exploited to crash a node
            panic!("Epoch being used for randomness is still open!");
        }

        // check if the block is valid
        if !block.is_valid(
            &self.merkle_root,
            &self.genesis_piece_hash,
            &epoch.randomness,
            &epoch.get_challenge_for_timeslot(block.proof.timeslot),
            &self.sloth,
        ) {
            // TODO: block list this peer
            return false;
        }

        true
    }

    /// Validates a proposer or tx block received via sync during startup
    pub async fn validate_block_from_sync(
        &mut self,
        block: &Block,
        current_timeslot: Timeslot,
    ) -> bool {
        // is this from the timeslot requested? else error
        if block.proof.timeslot != current_timeslot {
            error!("Received a block via sync for block at incorrect timeslot");
            return false;
        }

        let proof_id = block.proof.get_id();

        // is this a new block?
        if self.recent_proof_ids.contains(&proof_id) {
            error!("Received a block proposal via sync for known block");
            return false;
        }

        // TODO: make this into a self-pruning data structure
        // else add
        self.recent_proof_ids.insert(proof_id);

        // if not the genesis block, get parent
        // TODO: this will need to be adjusted once we are solving from genesis
        if block.proof.timeslot > 0 && block.content.parent_id.is_some() {
            // have we already received parent? else error
            if !self
                .metablocks
                .contains_content_id(&block.content.parent_id.expect("Is proposer block"))
            {
                error!("Received a block via sync with unknown parent");
                return false;
            }

            let parent_metablock = self.metablocks.get_metablock_from_content_id(
                &block.content.parent_id.expect("Already checked above"),
            );

            // ensure the parent is from an earlier timeslot
            if parent_metablock.block.proof.timeslot >= block.proof.timeslot {
                error!("Received a block via sync whose parent is in the future");
                return false;
            }

            // is the parent not too far back? (no deep forks)
            // compare parent block height to current block height of longest chain
            if parent_metablock.height + (CONFIRMATION_DEPTH as u64) < self.heads[0].block_height {
                error!("Received a block via sync that would cause a deep fork");
                return false;
            }
        }

        if block.proof.timeslot >= (CHALLENGE_LOOKBACK_EPOCHS * TIMESLOTS_PER_EPOCH) {
            // block is valid?
            if !(self.validate_block(block).await) {
                return false;
            }
        }

        true
    }

    /// Validates a proposer or tx block received via gossip
    pub async fn validate_block_from_gossip(&mut self, block: &Block) -> bool {
        debug!(
            "Validating remote block for epoch: {} at timeslot {}",
            block.proof.epoch, block.proof.timeslot
        );

        let proof_id = block.proof.get_id();

        // TODO: handle the case where we get two different content_ids for the same proof_id
        // is this a new block?
        if self.recent_proof_ids.contains(&proof_id) {
            warn!("Received a block proposal via gossip for known block, ignoring");
            return false;
        }

        // TODO: make this into a self-pruning data structure
        // else add
        self.recent_proof_ids.insert(proof_id);

        // If node is still syncing the ledger and is a proposer block, cache and apply on sync
        if !self.timer_is_running {
            if block.content.parent_id.is_some() {
                warn!("Caching a proposer block received via gossip before the ledger is synced");
                self.cache_remote_proposer_block(block);
            } else {
                warn!("Caching a tx block received via gossip before the ledger is synced");
                self.cached_tx_blocks_by_content_id
                    .insert(block.content.get_id(), block.clone());
            }
            return false;
        }

        // has the proof's timeslot arrived?
        if self.current_timeslot < block.proof.timeslot {
            if self.current_timeslot - MAX_EARLY_TIMESLOTS > block.proof.timeslot {
                // TODO: flag this peer
                error!("Ignoring a block that is too early");
                return false;
            }

            // else cache and wait for arrival
            self.early_blocks_by_timeslot
                .entry(block.proof.timeslot)
                .and_modify(|blocks| blocks.push(block.clone()))
                .or_insert(vec![block.clone()]);

            warn!("Caching a block that is early");
            return false;
        }

        // is the timeslot recent enough?
        if block.proof.timeslot > self.current_timeslot + MAX_LATE_TIMESLOTS {
            // TODO: flag this peer
            error!("Received a late block via gossip, ignoring");
            return false;
        }

        // if a proposer block, validate parent
        if block.content.parent_id.is_some() {
            // If we are not aware of the blocks parent, cache and apply once parent is seen
            if !self
                .metablocks
                .contains_content_id(&block.content.parent_id.expect("Already checked above"))
            {
                panic!(
                    "Caching a block received via gossip with unknown parent content id: {}",
                    hex::encode(&block.content.parent_id.expect("Is Some")[0..8])
                );
                self.cache_remote_proposer_block(block);
                return false;
            }

            let parent_metablock = self.metablocks.get_metablock_from_content_id(
                &block.content.parent_id.expect("Already checked above"),
            );

            // ensure the parent is from an earlier timeslot
            if parent_metablock.block.proof.timeslot >= block.proof.timeslot {
                // TODO: blacklist this peer
                error!("Ignoring a block whose parent is in the future");
                return false;
            }

            // is the parent not too far back? (no deep forks)
            // compare parent block height to current block height of longest chain
            if parent_metablock.height + (CONFIRMATION_DEPTH as u64) < self.heads[0].block_height {
                // TODO: blacklist this peer
                error!("Ignoring a block that would cause a deep fork");
                return false;
            }
        }

        // is the block valid?
        if !(self.validate_block(block).await) {
            return false;
        }

        true
    }

    /// Completes validation for a cached proposer block received via gossip whose parent has been staged
    pub async fn validate_block_from_cache(&mut self, block: &Block) -> bool {
        if block.content.parent_id.is_some() {
            // is parent from earlier timeslot?
            let parent_metablock = self.metablocks.get_metablock_from_content_id(
                &block.content.parent_id.expect("Already checked above"),
            );

            // ensure the parent is from an earlier timeslot
            if parent_metablock.block.proof.timeslot >= block.proof.timeslot {
                // TODO: blacklist this peer
                error!("Ignoring a block whose parent is in the future");
                return false;
            }

            // removed check for arrival time window, as this doesn't seem to apply to cached blocks
        }

        // is the block valid?
        if !(self.validate_block(block).await) {
            return false;
        }

        true
    }

    /// Completes validation for a proposer block received via gossip that was ahead of the timeslot received in and has now arrived
    pub async fn validate_block_that_has_arrived(&mut self, block: &Block) -> bool {
        // block is valid
        if !(self.validate_block(block).await) {
            return false;
        }

        // only for proposer blocks
        if block.content.parent_id.is_some() {
            // If we are not aware of the blocks parent, cache and apply once parent is seen
            if !self
                .metablocks
                .contains_content_id(&block.content.parent_id.expect("Already checked above"))
            {
                panic!(
                    "Caching a block received via gossip with unknown parent content id: {}",
                    hex::encode(&block.content.parent_id.expect("Is Some")[0..8])
                );
                self.cache_remote_proposer_block(block);
                return false;
            }

            let parent_metablock = self.metablocks.get_metablock_from_content_id(
                &block.content.parent_id.expect("Already checked above"),
            );

            // ensure the parent is from an earlier timeslot
            if parent_metablock.block.proof.timeslot >= block.proof.timeslot {
                // TODO: blacklist this peer
                error!("Ignoring a block whose parent is in the future");
                return false;
            }

            // is the parent not too far back? (no deep forks)
            // compare parent block height to current block height of longest chain
            if parent_metablock.height + (CONFIRMATION_DEPTH as u64) < self.heads[0].block_height {
                // TODO: blacklist this peer
                error!("Ignoring a block that would cause a deep fork");
                return false;
            }
        }

        true
    }

    /// Stage a new valid proposer block and confirm its k-deep parent
    pub async fn stage_proposer_block(&mut self, block: &Block) {
        info!(
            "Staging a new proposer block with {} tx blocks",
            block.content.refs.len() - 1
        );

        let mut parent_content_id = block
            .content
            .parent_id
            .expect("Proposers always have a parent");

        // TODO: this should be hardcoded into the reference implementation
        if self.genesis_timestamp == 0 {
            self.genesis_timestamp = block.content.timestamp;
        }

        // save block -> metablocks, blocks by timeslot
        let metablock = self.metablocks.save(block.clone());
        self.add_block_to_timeslot(block.proof.timeslot, metablock.proof_id);

        // save the coinbase tx
        self.txs.insert(
            block.coinbase_tx.get_id(),
            Transaction::Coinbase(block.coinbase_tx.clone()),
        );

        // for each tx block, remove from unclaimed or add to unknown
        for tx_block_id in block.content.refs.iter().skip(1) {
            if self.metablocks.contains_content_id(tx_block_id) {
                self.unclaimed_tx_block_ids.remove(tx_block_id);
            } else {
                self.unknown_tx_block_ids.insert(*tx_block_id);
            }
        }

        let proof_id = block.proof.get_id();

        // TODO: make sure the branch will not be below the current confirmed block height

        // check if block received during sync is in cached gossip and remove
        if self
            .cached_proposer_blocks_by_parent_content_id
            .contains_key(&proof_id)
        {
            // remove from cached gossip
            let mut is_empty = false;
            self.cached_proposer_blocks_by_parent_content_id
                .entry(parent_content_id)
                .and_modify(|blocks| {
                    blocks
                        .iter()
                        .position(|block| block.proof.get_id() == proof_id)
                        .map(|index| blocks.remove(index));
                    is_empty = blocks.is_empty();
                });

            if is_empty {
                self.cached_proposer_blocks_by_parent_content_id
                    .remove(&parent_content_id);
            }
        }

        // update head of this branch
        self.update_heads(parent_content_id, metablock.content_id, metablock.height);

        // confirm the k-deep parent
        let mut confirmation_depth = 0;
        loop {
            match self
                .metablocks
                .get_metablock_from_content_id_as_option(&parent_content_id)
            {
                Some(parent_block) => {
                    confirmation_depth += 1;
                    parent_content_id = parent_block
                        .block
                        .content
                        .parent_id
                        .expect("Is proposer block");
                    if confirmation_depth == CONFIRMATION_DEPTH {
                        self.confirm_block(&parent_block).await;
                        break;
                    }
                }
                None => break,
            }
        }
    }

    /// Stage a new valid transaction block
    pub async fn stage_tx_block(&mut self, block: &Block) {
        info!("Staging a new tx block");

        // TODO: this only has to be checked when the timer is not running
        // check if block received during sync is in cached gossip and remove
        if self
            .cached_tx_blocks_by_content_id
            .contains_key(&block.content.get_id())
        {
            // remove from cached gossip
            self.cached_tx_blocks_by_content_id
                .remove(&block.content.get_id());
        }

        // save block -> metablocks, blocks by timeslot
        let metablock = self.metablocks.save(block.clone());
        self.add_block_to_timeslot(block.proof.timeslot, metablock.proof_id);

        // save the coinbase tx
        self.txs.insert(
            block.coinbase_tx.get_id(),
            Transaction::Coinbase(block.coinbase_tx.clone()),
        );

        // remove from unknown or add to claimed tx blocks
        if self.unknown_tx_block_ids.contains(&metablock.content_id) {
            self.unknown_tx_block_ids.remove(&metablock.content_id);
        } else {
            self.unclaimed_tx_block_ids.insert(metablock.content_id);
        }

        // add to unknown or remove from claimed txs
        for tx_id in block.content.refs.iter().skip(1) {
            if self.txs.contains_key(tx_id) {
                self.unclaimed_tx_ids.remove(tx_id);
            } else {
                self.unknown_tx_ids.insert(*tx_id);
            }
        }
    }

    /// Stage all cached descendants for a given parent proposer block
    pub async fn stage_cached_children(&mut self, parent_id: ContentId) {
        let mut blocks = self
            .cached_proposer_blocks_by_parent_content_id
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();

        while blocks.len() > 0 {
            let mut additional_blocks: Vec<Block> = Vec::new();
            for block in blocks.drain(..) {
                if self.validate_block_from_cache(&block.clone()).await {
                    self.stage_proposer_block(&block.clone()).await;

                    self.cached_proposer_blocks_by_parent_content_id
                        .get(&block.content.get_id())
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .for_each(|block| additional_blocks.push(block.clone()));
                }
            }

            std::mem::swap(&mut blocks, &mut additional_blocks);
        }
    }

    /// Applies the txs in a block to balances when it is k-deep
    async fn confirm_block(&mut self, proposer_metablock: &MetaBlock) -> bool {
        // TODO: ensure there are no other confirmed blocks at this height

        // ensure this block has not already been confirmed
        if self.confirmed_blocks.contains(&proposer_metablock.proof_id) {
            debug!("Staged block references a block that has already been confirmed");
            return false;
        }

        // TODO: do we need to account for the timeslot offset here?
        // TODO: this should be a separate function
        // update the solution range on Eon boundary
        if proposer_metablock.height > GENESIS_OFFSET
            && (proposer_metablock.height - GENESIS_OFFSET) % PROPOSER_BLOCKS_PER_EON == 0
        {
            /* Hypothesize as to why expected and actual differ...
             * as plot size increases, variance does not appear to change
             * as timeslots_per_block increases, adjustment decreases
             * as blocks_per_eon increase, variance decreases
             * as the size of the domain increases, does the variance decrease?
             * reducing the lag between calculation and implementation
             * all of these things are amplifying each other
             */

            // a new eon has arrived
            let elapsed_timeslots = self.current_timeslot - self.last_eon_close_timeslot;
            self.last_eon_close_timeslot = self.current_timeslot;
            let mut range_adjustment = elapsed_timeslots as f64 / EXPECTED_TIMESLOTS_PER_EON as f64;

            // ensure the range does not change more than a factor 4
            if range_adjustment > 4.0f64 {
                range_adjustment = 4.0f64;
            }

            if range_adjustment < 0.25f64 {
                range_adjustment = 0.25f64;
            }

            // stage the new solution range for update after timeslots expire
            self.solution_range_update = SolutionRangeUpdate::new(
                self.current_timeslot + SOLUTION_RANGE_UPDATE_DELAY_IN_TIMESLOTS,
                proposer_metablock.height,
                (self.current_solution_range as f64 * range_adjustment) as u64,
            );

            warn!(
                "Eon has closed.
                Expected timeslots elapsed is: {}
                Actual timeslots elapsed is: {}
                Range adjustment is: {}",
                EXPECTED_TIMESLOTS_PER_EON, elapsed_timeslots, range_adjustment,
            )
        }

        info!(
            "Confirmed block with height {} at timeslot {}",
            proposer_metablock.height, self.current_timeslot
        );

        // first have to make sure that we have all tx blocks and txs, else we discard the block
        // also need to ensure that we have exactly one coinbase tx per block
        // if we already have applied the tx block we can skip checking for those txs
        for (index, ref_id) in proposer_metablock.block.content.refs.iter().enumerate() {
            // first ref is always the coinbase tx
            if index == 0 {
                match self.txs.get(ref_id) {
                    Some(tx) => match tx {
                        Transaction::Coinbase(_) => {}
                        Transaction::Credit(_) => {
                            error!("Cannot confirm proposer block, first ref is not a coinbase tx");
                            return false;
                        }
                    },
                    None => {
                        error!("Cannot confirm proposer block, first ref is an unknown tx");
                        return false;
                    }
                }
            } else {
                // remaining refs are tx blocks

                // has this tx block already been applied?
                if self.applied_tx_block_ids.contains(ref_id) {
                    warn!("Tx block has already been applied applied by a previous proposer blocks, skipping");
                    continue;
                }

                // get the tx block from metablocks and check for all txs
                match self
                    .metablocks
                    .get_metablock_from_content_id_as_option(ref_id)
                {
                    Some(tx_block) => {
                        // do we have all txs referenced?
                        for (index, tx_id) in tx_block.block.content.refs.iter().enumerate() {
                            match self.txs.get(tx_id) {
                                Some(tx) => {
                                    match tx {
                                        Transaction::Coinbase(_) => {
                                            if index != 0 {
                                                error!(
                                                    "Cannot confirm proposer block, tx block does not have a coinbase tx"
                                                );
                                                return false;
                                            }
                                        }
                                        Transaction::Credit(_) => {
                                            if index == 0 {
                                                error!(
                                                    "Cannot confirm proposer block, tx block has multiple coinbase txs"
                                                );
                                                return false;
                                            }
                                        }
                                    };
                                }
                                None => {
                                    error!(
                                        "Cannot confirm proposer block, tx block refs an unknown tx"
                                    );
                                    return false;
                                }
                            }
                        }
                    }
                    None => {
                        error!("Cannot confirm proposer block, includes unknown tx block");
                        return false;
                    }
                }
            }
        }

        // for each tx block
        // coinbase tx
        // remaining txs by hash
        // proposer block coinbase tx
        // filter out txs that have been applied

        // order un-applied tx blocks by timeslot then by proof id
        let mut tx_blocks_by_height: BTreeMap<Timeslot, Vec<ProofId>> = BTreeMap::new();
        proposer_metablock
            .block
            .content
            .refs
            .iter()
            .skip(1)
            .filter(|content_id| !self.applied_tx_block_ids.contains(*content_id))
            .for_each(|content_id| {
                let tx_block = self
                    .metablocks
                    .get_metablock_from_content_id(content_id)
                    .block;

                tx_blocks_by_height
                    .entry(tx_block.proof.timeslot)
                    .and_modify(|tx_blocks| {
                        tx_blocks.push(tx_block.proof.get_id());
                        tx_blocks.sort();
                    })
                    .or_insert(vec![tx_block.proof.get_id()]);
            });

        tx_blocks_by_height
            .values()
            .flatten()
            .for_each(|tx_block_proof_id| {
                let tx_block = self
                    .metablocks
                    .get_metablock_from_proof_id(tx_block_proof_id)
                    .block;

                // validate and apply each tx
                for tx_id in tx_block.content.refs.iter() {
                   match self.txs.get(tx_id).expect("Already checked") {
                       Transaction::Coinbase(tx) => {
                           // create or update account state
                           self.balances
                               .entry(tx.to_address)
                               .and_modify(|account_state| account_state.balance += BLOCK_REWARD)
                               .or_insert(AccountState {
                                   nonce: 0,
                                   balance: BLOCK_REWARD,
                               });

                           debug!("Applied a coinbase tx to balances");
                       }
                       Transaction::Credit(tx) => {
                           // check if the tx has already been applied
                           let tx_id = tx.get_id();
                           if self.applied_tx_ids.contains(&tx_id) {
                               warn!(
                                   "Transaction has already been referenced by a previous block, skipping"
                               );
                               continue;
                           }

                           // ensure the tx is still valid
                           let sender_account_state = self
                               .balances
                               .get(&tx.from_address)
                               .expect("Existence of account state has already been validated");

                           if sender_account_state.balance < tx.amount {
                               error!("Invalid transaction, from account state has insufficient funds, transaction will not be applied");
                               continue;
                           }

                           if sender_account_state.nonce >= tx.nonce {
                               error!("Invalid transaction, tx nonce has already been used, transaction will not be applied");
                               continue;
                           }

                           // debit the sender
                           self.balances
                               .entry(tx.from_address)
                               .and_modify(|account_state| account_state.balance -= tx.amount);

                           // credit  the receiver
                           self.balances
                               .entry(tx.to_address)
                               .and_modify(|account_state| account_state.balance += tx.amount)
                               .or_insert(AccountState {
                                   nonce: 0,
                                   balance: tx.amount,
                               });

                           // TODO: pay tx fee to farmer
                       }
                   }

                    // TODO: add tx to state buffer

                    // TODO: what do we do with invalid txs?
                    // normally we would just include them but not apply them
                    // then someone could upload a storage tx for free
                    // if we don't include the content we could deal with that

                    // track each applied tx
                    self.applied_tx_ids.insert(*tx_id);
                }

                // TODO: add each tx block header to state buffer

                // track each applied tx block
                self.applied_tx_block_ids.insert(tx_block.content.get_id());
            });

        // add in the proposer block coinbase tx
        match self
            .txs
            .get(&proposer_metablock.block.content.refs[0])
            .expect("Already checked")
        {
            Transaction::Coinbase(tx) => {
                self.balances
                    .entry(tx.to_address)
                    .and_modify(|account_state| account_state.balance += BLOCK_REWARD)
                    .or_insert(AccountState {
                        nonce: 0,
                        balance: BLOCK_REWARD,
                    });

                self.applied_tx_ids.insert(tx.get_id());
                // TODO: add to state
            }
            Transaction::Credit(_) => {
                error!("First ref in proposer block must be a coinbase tx");
            }
        }

        // TODO: add proposer block header to state buffer

        self.confirmed_blocks.insert(proposer_metablock.proof_id);

        // add to epoch tracker
        self.epoch_tracker
            .add_block_to_epoch(
                proposer_metablock.block.proof.epoch,
                proposer_metablock.height,
                proposer_metablock.proof_id,
            )
            .await;

        // prune any siblings of this block
        if proposer_metablock.height > 0 {
            let siblings = self
                .metablocks
                .get_metablock_from_content_id(
                    &proposer_metablock
                        .block
                        .content
                        .parent_id
                        .expect("Only proposer blocks are confirmed"),
                )
                .children
                .drain_filter(|proof_id| proof_id != &proposer_metablock.proof_id)
                .collect();

            self.prune_blocks_recursive(siblings);
        }

        // TODO: update chain quality

        true
    }

    /// Recursively removes all siblings and their descendants when a new block is confirmed
    fn prune_blocks_recursive(&mut self, proof_ids: Vec<ProofId>) {
        for child_proof_id in proof_ids.iter() {
            let metablock = self.metablocks.remove(child_proof_id);

            // remove from blocks by timeslot
            self.proof_ids_by_timeslot
                .entry(metablock.block.proof.timeslot)
                .and_modify(|proof_ids| {
                    let index = proof_ids
                        .iter()
                        .position(|proof_id| proof_id == metablock.proof_id.as_ref())
                        .unwrap();
                    proof_ids.remove(index);
                });

            if metablock.children.len() > 0 {
                // repeat with this blocks children
                self.prune_blocks_recursive(metablock.children);
            } else {
                // leaf node, remove the branch from heads
                self.prune_branch(metablock.content_id);
            }
        }
    }

    /// cache a block received via gossip ahead of the current epoch
    /// block will be staged once it's parent is seen
    fn cache_remote_proposer_block(&mut self, block: &Block) {
        self.cached_proposer_blocks_by_parent_content_id
            .entry(
                block
                    .content
                    .parent_id
                    .expect("only called on proposer blocks"),
            )
            .and_modify(|blocks| blocks.push(block.clone()))
            .or_insert(vec![block.clone()]);
    }

    /// Retrieve the balance for a given node id
    fn _get_account_state(&self, id: &[u8]) -> Option<AccountState> {
        self.balances.get(id).copied()
    }

    /// Print the balance of all accounts in the ledger
    fn _print_balances(&self) {
        info!("Current balance of accounts:\n");
        for (id, account_state) in self.balances.iter() {
            info!(
                "Account: {} \t {} \t credits",
                hex::encode(id),
                account_state.balance
            );
        }
    }
}
