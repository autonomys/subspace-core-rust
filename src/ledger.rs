#![allow(dead_code)]

use super::*;
use crate::farmer::FarmerMessage;
use crate::timer::EpochTracker;
use async_std::sync::Sender;
use block::{Block, Content, Data, Proof};
use farmer::Solution;
use log::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/* ToDo
 * ----
 *
 * Make difficulty self-adjusting
 * Track chain quality
 * Track parent links to order blocks and transactions
 *
 * Commits to the ledger should be atmoic (if we fail part way through)
 *
 * TESTING
 * -------
 * Piece count is always 256 for testing for the merkle tree
 * Plot size is configurable, but must be a multiple of 256
 * For each challenge, solver will check every 256th piece starting at index and return the top N
 * We want to start with 256 x 256 pieces
 * This mean for each challenge the expected quality should be below 2^32 / 2^8 -> 2^24
 *
*/

pub struct Ledger {
    balances: HashMap<[u8; 32], usize>,
    pub genesis_timestamp: u64,
    pub genesis_piece_hash: [u8; 32],
    pub blocks: HashMap<BlockId, Block>,
    pub unseen_block_ids: HashSet<BlockId>,
    // TODO: add ordered confirmed_blocks_by_block_height
    pub confirmed_blocks_by_timeslot: HashMap<u64, Vec<BlockId>>,
    pub cached_blocks_for_timeslot: BTreeMap<u64, Vec<BlockId>>,
    pub epoch_tracker: EpochTracker,
    pub timer_is_running: bool,
    pub height: u32,
    pub quality: u32,
    pub keys: ed25519_dalek::Keypair,
    pub sloth: sloth::Sloth,
    pub merkle_root: Vec<u8>,
    pub merkle_proofs: Vec<Vec<u8>>,
    pub tx_payload: Vec<u8>,
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

        Ledger {
            blocks: HashMap::new(),
            epoch_tracker,
            confirmed_blocks_by_timeslot: HashMap::new(),
            cached_blocks_for_timeslot: BTreeMap::new(),
            unseen_block_ids: HashSet::new(),
            genesis_timestamp: 0,
            timer_is_running: false,
            balances: HashMap::new(),
            height: 0,
            quality: 0,
            merkle_root,
            genesis_piece_hash,
            sloth,
            keys,
            tx_payload,
            merkle_proofs,
        }
    }

    /// Retrieve all blocks for a timeslot, return an empty vec if no blocks
    pub fn get_blocks_by_timeslot(&self, timeslot: u64) -> Vec<Block> {
        self.confirmed_blocks_by_timeslot
            .get(&timeslot)
            .map(|blocks| {
                blocks
                    .iter()
                    .map(|block_id| self.blocks.get(block_id).unwrap().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Start a new chain from genesis as a gateway node
    pub async fn init_from_genesis(&mut self) {
        self.genesis_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;

        let mut timestamp = self.genesis_timestamp as u64;
        let mut parent_id: BlockId = [0u8; 32];

        for _ in 0..CHALLENGE_LOOKBACK {
            let current_epoch = self.epoch_tracker.advance_epoch().await;
            info!("Advanced to epoch {} during genesis init", current_epoch);

            for current_timeslot in (0..TIMESLOTS_PER_EPOCH)
                .map(|timeslot_index| timeslot_index + current_epoch * TIMESLOTS_PER_EPOCH)
            {
                let proof = Proof {
                    randomness: self.genesis_piece_hash,
                    epoch: current_epoch,
                    timeslot: current_timeslot,
                    public_key: self.keys.public.to_bytes(),
                    tag: 0,
                    nonce: 0,
                    piece_index: 0,
                };

                let mut content = Content {
                    parent_ids: vec![parent_id],
                    proof_id: proof.get_id(),
                    proof_signature: self.keys.sign(&proof.get_id()).to_bytes().to_vec(),
                    timestamp,
                    tx_ids: Vec::new(),
                    signature: Vec::new(),
                };

                content.signature = self.keys.sign(&content.get_id()).to_bytes().to_vec();

                let data = Data {
                    encoding: Vec::new(),
                    merkle_proof: Vec::new(),
                };

                let block = Block {
                    proof,
                    content,
                    data: Some(data),
                };

                self.apply_genesis_block(&block, current_timeslot);
                parent_id = block.get_id();

                self.epoch_tracker
                    .add_block_to_epoch(current_epoch, current_timeslot, parent_id)
                    .await;

                info!(
                    "Applied a genesis block to ledger with id {}",
                    hex::encode(&parent_id)
                );
                let time_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_millis();

                timestamp += TIMESLOT_DURATION;

                async_std::task::sleep(Duration::from_millis(timestamp - time_now as u64)).await;
            }
        }

        self.epoch_tracker.advance_epoch().await;

        self.unseen_block_ids.insert(parent_id);
    }

    /// apply each genesis block to the ledger
    pub fn apply_genesis_block(&mut self, block: &Block, timeslot: u64) {
        let block_id = block.get_id();
        let mut pruned_block = block.clone();
        pruned_block.prune();
        self.blocks.insert(block_id, pruned_block);

        // update height
        self.height += 1;

        // Adds a pointer to this block id for the given timeslot in the ledger
        self.confirmed_blocks_by_timeslot
            .entry(timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);
    }

    /// create a new block locally from a valid farming solution
    pub async fn create_and_apply_local_block(
        &mut self,
        solution: Option<Solution>,
    ) -> Option<Block> {
        match solution {
            Some(solution) => {
                let proof = Proof {
                    randomness: solution.randomness,
                    epoch: solution.epoch,
                    timeslot: solution.timeslot,
                    public_key: self.keys.public.to_bytes(),
                    tag: solution.tag,
                    nonce: 0,
                    piece_index: solution.piece_index,
                };
                let data = Data {
                    encoding: solution.encoding.to_vec(),
                    merkle_proof: crypto::get_merkle_proof(
                        solution.proof_index,
                        &self.merkle_proofs,
                    ),
                };
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_millis() as u64;
                // must get parents from the chain
                // TODO: only get parents that are not at the same level
                // TODO: create an empty vec then do memswap between unseen parent and unseen block ids
                let unseen_parents: Vec<BlockId> = self.unseen_block_ids.drain().collect();
                let mut content = Content {
                    parent_ids: unseen_parents,
                    proof_id: proof.get_id(),
                    proof_signature: self.keys.sign(&proof.get_id()).to_bytes().to_vec(),
                    timestamp,
                    tx_ids: self.tx_payload.clone(),
                    signature: Vec::new(),
                };
                content.signature = self.keys.sign(&content.get_id()).to_bytes().to_vec();
                let block = Block {
                    proof,
                    content,
                    data: Some(data),
                };

                // from here, code is shared with validate_and_apply block

                // get correct randomness for this block
                let epoch = self
                    .epoch_tracker
                    .get_lookback_epoch(block.proof.epoch)
                    .await;

                // let challenge_timeslot =
                //     block.proof.timeslot - CHALLENGE_LOOKBACK * TIMESLOTS_PER_EPOCH;

                if !epoch.is_closed {
                    panic!("Epoch being used for randomness is still open!");
                }

                // check if the block is valid
                block.is_valid(
                    &self.merkle_root,
                    &self.genesis_piece_hash,
                    &epoch.randomness,
                    &epoch.get_challenge_for_timeslot(block.proof.timeslot),
                    &self.sloth,
                );

                let block_id = block.get_id();
                self.unseen_block_ids.insert(block_id);
                // apply the block to the ledger

                let mut pruned_block = block.clone();
                pruned_block.prune();
                self.blocks.insert(block_id, pruned_block);
                // TODO: update chain quality
                // update balances, get or add account
                self.balances
                    .entry(crypto::digest_sha_256(&block.proof.public_key))
                    .and_modify(|balance| *balance += 1)
                    .or_insert(1);
                // Adds a pointer to this block id for the given timeslot in the ledger
                self.confirmed_blocks_by_timeslot
                    .entry(solution.timeslot)
                    .and_modify(|block_ids| block_ids.push(block_id))
                    .or_insert(vec![block_id]);

                // TODO: collect all blocks for a slot, then order blocks, then order tx

                warn!("Adding block to epoch during create and apply local block");

                // update the epoch for this block
                self.epoch_tracker
                    .add_block_to_epoch(block.proof.epoch, block.proof.timeslot, block_id)
                    .await;

                // info!("Applied block to ledger at timeslot: {}", solution.timeslot);
                return Some(block);
            }
            None => {
                return None;
            }
        };
    }

    /// cache a block received via gossip ahead of the current epoch
    pub fn cache_remote_block(&mut self, block: Block) {
        // cache the block
        let block_id = block.get_id();
        self.blocks.insert(block_id, block.clone());

        // add to cached blocks tracker
        self.cached_blocks_for_timeslot
            .entry(block.proof.timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);
    }

    /// validate and apply a block received via gossip
    pub async fn validate_and_apply_remote_block(&mut self, block: Block) -> bool {
        info!(
            "Validating and applying block for epoch: {} at timeslot {}",
            block.proof.epoch, block.proof.timeslot
        );

        // get correct randomness for this block
        let epoch = self
            .epoch_tracker
            .get_lookback_epoch(block.proof.epoch)
            .await;

        if !epoch.is_closed {
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
            return false;
        }

        // TODO: from here on the code is shared with create_and_apply_local

        let block_id = block.get_id();

        self.unseen_block_ids.insert(block_id);
        // apply the block to the ledger

        // remove the block data
        let mut pruned_block = block.clone();
        pruned_block.prune();

        self.blocks.insert(block_id, pruned_block);
        // TODO: update chain quality
        // update balances, get or add account
        self.balances
            .entry(crypto::digest_sha_256(&block.proof.public_key))
            .and_modify(|balance| *balance += 1)
            .or_insert(1);
        // Adds a pointer to this block id for the given timeslots in the ledger
        self.confirmed_blocks_by_timeslot
            .entry(block.proof.timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);

        // TODO: collect all blocks for a slot, then order blocks, then order tx

        // update the epoch for this block
        self.epoch_tracker
            .add_block_to_epoch(block.proof.epoch, block.proof.timeslot, block_id)
            .await;

        // TODO: apply children of this block that were depending on it

        true
    }

    /// validate a block received via sync from another node
    pub async fn apply_block_from_sync(&mut self, block: Block) {
        if self.genesis_timestamp == 0 {
            self.genesis_timestamp = block.content.timestamp;
        }
        let block_id = block.get_id();
        self.blocks.insert(block_id, block.clone());

        // check if the block is in pending gossip and remove
        {
            let mut is_empty = false;
            self.cached_blocks_for_timeslot
                .entry(block.proof.timeslot)
                .and_modify(|block_ids| {
                    block_ids
                        .iter()
                        .position(|blk_id| *blk_id == block_id)
                        .map(|index| block_ids.remove(index));
                    is_empty = block_ids.is_empty();
                });
            if is_empty {
                self.cached_blocks_for_timeslot
                    .remove(&block.proof.timeslot);
            }
        }

        // TODO: update chain quality

        block.content.parent_ids.iter().for_each(|parent_id| {
            self.unseen_block_ids.remove(parent_id);
        });

        self.unseen_block_ids.insert(block_id);

        // if not a genesis block, count block reward
        if block.proof.randomness != self.genesis_piece_hash {
            // update balances, get or add account
            self.balances
                .entry(crypto::digest_sha_256(&block.proof.public_key))
                .and_modify(|balance| *balance += 1)
                .or_insert(1);
        }
        // Adds a pointer to this block id for the given timelsot in the ledger
        self.confirmed_blocks_by_timeslot
            .entry(block.proof.timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);

        info!(
            "Applied new block during sync at timeslot: {}",
            block.proof.timeslot
        );

        // have to update the epoch and close on boundaries
        self.epoch_tracker
            .add_block_to_epoch(block.proof.epoch, block.proof.timeslot, block_id)
            .await;
    }

    /// validate and apply a cached block from gossip after sycing the ledger
    pub async fn validate_and_apply_cached_block(&mut self, block: Block) -> bool {
        //TODO: must handle the case where the epoch is still open

        let randomness_epoch_index = block.proof.epoch - CHALLENGE_LOOKBACK;
        let challenge_timeslot = block.proof.timeslot;
        info!(
            "Validating and applying cached block for epoch: {} at timeslot {}",
            randomness_epoch_index, challenge_timeslot
        );

        // get correct randomness for this block
        let epoch = self.epoch_tracker.get_epoch(randomness_epoch_index).await;

        // check if the block is valid
        if !block.is_valid(
            &self.merkle_root,
            &self.genesis_piece_hash,
            &epoch.randomness,
            &epoch.get_challenge_for_timeslot(challenge_timeslot),
            &self.sloth,
        ) {
            return false;
        }

        // TODO: from here on the code is shared with create_and_apply_local

        let block_id = block.get_id();

        self.unseen_block_ids.insert(block_id);
        // apply the block to the ledger

        // remove the block data
        let mut pruned_block = block.clone();
        pruned_block.prune();

        self.blocks.insert(block_id, pruned_block);
        // TODO: update chain quality
        // update balances, get or add account
        self.balances
            .entry(crypto::digest_sha_256(&block.proof.public_key))
            .and_modify(|balance| *balance += 1)
            .or_insert(1);
        // Adds a pointer to this block id for the given timelsot in the ledger
        self.confirmed_blocks_by_timeslot
            .entry(block.proof.timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);

        true
    }

    /// apply the cached block to the ledger
    pub async fn apply_cached_blocks(&mut self, timeslot: u64) -> bool {
        for current_timeslot in timeslot.. {
            if let Some(block_ids) = self.cached_blocks_for_timeslot.remove(&current_timeslot) {
                for block_id in block_ids.iter() {
                    let cached_block = self.blocks.get(block_id).unwrap().clone();
                    if !self.validate_and_apply_cached_block(cached_block).await {
                        return false;
                    }
                }
            }

            if current_timeslot % TIMESLOTS_PER_EPOCH as u64 == 0 {
                // create the new epoch
                let current_epoch = self.epoch_tracker.advance_epoch().await;

                info!(
                    "Closed randomness for epoch {} during apply cached blocks",
                    current_epoch - CHALLENGE_LOOKBACK
                );

                info!("Creating a new empty epoch for epoch {}", current_epoch);
            }

            if self.cached_blocks_for_timeslot.is_empty() {
                break;
            }
        }

        true
    }

    /// start the timer after syncing the ledger
    pub async fn start_timer_from_genesis_time(
        &mut self,
        timer_to_farmer_tx: Sender<FarmerMessage>,
        is_farming: bool,
    ) {
        info!("Starting the timer from genesis time");
        let genesis_block_id: BlockId = self.confirmed_blocks_by_timeslot.get(&0).unwrap()[0];
        let genesis_block = self.blocks.get(&genesis_block_id).unwrap().clone();
        let genesis_time = genesis_block.content.timestamp as u64;
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;
        let elapsed_time = current_time - genesis_time;
        let mut elapsed_timeslots = elapsed_time / TIMESLOT_DURATION;
        let time_to_next_timeslot = (TIMESLOT_DURATION * (elapsed_timeslots + 1)) - elapsed_time;

        self.timer_is_running = true;

        async_std::task::sleep(Duration::from_millis(time_to_next_timeslot)).await;
        elapsed_timeslots += 1;
        let epoch_tracker = self.epoch_tracker.clone();
        let genesis_timestamp = self.genesis_timestamp;
        async_std::task::spawn(async move {
            timer::run(
                timer_to_farmer_tx,
                epoch_tracker,
                elapsed_timeslots as u64,
                is_farming,
                genesis_timestamp,
            )
            .await;
        });
    }

    /// Retrieve the balance for a given node id
    pub fn get_balance(&self, id: &[u8]) -> Option<usize> {
        self.balances.get(id).copied()
    }

    /// Print the balance of all accounts in the ledger
    pub fn print_balances(&self) {
        info!("Current balance of accounts:\n");
        for (id, balance) in self.balances.iter() {
            info!("Account: {} \t {} \t credits", hex::encode(id), balance);
        }
    }

    pub fn get_block_height(&self) -> u32 {
        // TODO: maybe this should be cached locally?
        self.height
    }
}

// #[cfg(test)]
// mod tests {

//     use super::*;
//     use std::time::{SystemTime, UNIX_EPOCH};

//     // #[test]
//     // fn block() {
//     //     let tx_payload = crypto::generate_random_piece().to_vec();
//     //     let block = Block::new(
//     //         SystemTime::now()
//     //             .duration_since(UNIX_EPOCH)
//     //             .expect("Time went backwards")
//     //             .as_millis(),
//     //         crypto::random_bytes_32(),
//     //         crypto::random_bytes_32(),
//     //         crypto::random_bytes_32(),
//     //         crypto::random_bytes_32(),
//     //         [0u8; 64].to_vec(),
//     //         tx_payload,
//     //     );
//     //     let block_id = block.get_id();
//     //     let block_vec = block.to_bytes();
//     //     let block_copy = Block::from_bytes(&block_vec).unwrap();
//     //     let block_copy_id = block_copy.get_id();
//     //     assert_eq!(block_id, block_copy_id);
//     // }

//     // #[test]
//     // fn auxillary_data() {
//     //     let encoding = crypto::generate_random_piece();
//     //     let (merkle_proofs, _) = crypto::build_merkle_tree();
//     //     let proof = Proof::new(encoding, merkle_proofs[17].clone(), 17u64, 245u64);
//     //     let proof_id = proof.get_id();
//     //     let proof_vec = proof.to_bytes();
//     //     let proof_copy = Proof::from_bytes(&proof_vec).unwrap();
//     //     let proof_copy_id = proof_copy.get_id();
//     //     assert_eq!(proof_id, proof_copy_id);
//     // }
// }
