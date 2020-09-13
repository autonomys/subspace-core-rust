use crate::block::{Block, Content, Data, Proof};
use crate::farmer::{FarmerMessage, Solution};
use crate::timer::EpochTracker;
use crate::{
    crypto, sloth, timer, BlockId, Tag, CHALLENGE_LOOKBACK_EPOCHS, PRIME_SIZE_BITS, SOLUTION_RANGE,
    TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION,
};
use async_std::sync::Sender;
use log::info;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::TryInto;
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

        for _ in 0..CHALLENGE_LOOKBACK_EPOCHS {
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
                    tag: Tag::default(),
                    // TODO: Fix this
                    nonce: u64::from_le_bytes(
                        crypto::create_hmac(&[], b"subspace")[0..8]
                            .try_into()
                            .unwrap(),
                    ),
                    piece_index: 0,
                    solution_range: SOLUTION_RANGE,
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

                // apply the block to the ledger
                self.apply_block(&block).await;

                parent_id = block.get_id();

                info!(
                    "Applied a genesis block to ledger with id {}",
                    hex::encode(&parent_id[0..8])
                );
                let time_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_millis();

                timestamp += TIMESLOT_DURATION;

                async_std::task::sleep(Duration::from_millis(timestamp - time_now as u64)).await;
            }
        }
    }

    /// Apply block to the ledger
    async fn apply_block(&mut self, block: &Block) {
        let block_id = block.get_id();
        let mut pruned_block = block.clone();
        pruned_block.prune();
        self.blocks.insert(block_id, pruned_block);

        self.unseen_block_ids.insert(block_id);

        block.content.parent_ids.iter().for_each(|parent_id| {
            self.unseen_block_ids.remove(parent_id);
        });

        // Adds a pointer to this block id for the given timeslot in the ledger
        self.confirmed_blocks_by_timeslot
            .entry(block.proof.timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);

        // TODO: update chain quality
        // Weight: actual blocks / expected blocks (eon)
        // smaller the range the higher the quality
        //

        // TODO: Why not genesis block is different?
        // if not a genesis block, count block reward
        if block.proof.randomness != self.genesis_piece_hash {
            // update balances, get or add account
            self.balances
                .entry(crypto::digest_sha_256(&block.proof.public_key))
                .and_modify(|balance| *balance += 1)
                .or_insert(1);
        }

        // update the epoch for this block
        self.epoch_tracker
            .add_block_to_epoch(
                block.proof.epoch,
                block.proof.timeslot,
                block.proof.get_id(),
                block.proof.solution_range,
            )
            .await;
    }

    /// create a new block locally from a valid farming solution
    pub async fn create_and_apply_local_block(&mut self, solution: Solution) -> Block {
        let proof = Proof {
            randomness: solution.randomness,
            epoch: solution.epoch,
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
            solution_range: solution.range,
        };
        let data = Data {
            encoding: solution.encoding.to_vec(),
            merkle_proof: crypto::get_merkle_proof(solution.proof_index, &self.merkle_proofs),
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
        let is_valid = block.is_valid(
            &self.merkle_root,
            &self.genesis_piece_hash,
            &epoch.randomness,
            &epoch.get_challenge_for_timeslot(block.proof.timeslot),
            &self.sloth,
        );
        assert!(is_valid, "Local block must always be valid");

        // apply the block to the ledger
        self.apply_block(&block).await;

        // TODO: collect all blocks for a slot, then order blocks, then order tx

        block
    }

    /// cache a block received via gossip ahead of the current epoch
    pub fn cache_remote_block(&mut self, block: Block) {
        // cache the block
        let block_id = block.get_id();
        // TODO: Does this need to be inserted here at all?
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

        // apply the block to the ledger
        self.apply_block(&block).await;

        // TODO: collect all blocks for a slot, then order blocks, then order tx

        // TODO: apply children of this block that were depending on it

        true
    }

    // TODO: Where is validation???
    /// validate a block received via sync from another node
    pub async fn apply_block_from_sync(&mut self, block: Block) {
        if self.genesis_timestamp == 0 {
            self.genesis_timestamp = block.content.timestamp;
        }
        let block_id = block.get_id();
        // apply the block to the ledger
        self.apply_block(&block).await;

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

        info!(
            "Applied new block during sync at timeslot: {}",
            block.proof.timeslot
        );
    }

    /// validate and apply a cached block from gossip after syncing the ledger
    pub async fn validate_and_apply_cached_block(&mut self, block: Block) -> bool {
        //TODO: must handle the case where the epoch is still open

        let randomness_epoch_index = block.proof.epoch - CHALLENGE_LOOKBACK_EPOCHS;
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

        // apply the block to the ledger
        self.apply_block(&block).await;

        true
    }

    /// apply the cached block to the ledger
    ///
    /// Returns last (potentially unfinished) timeslot
    pub async fn apply_cached_blocks(&mut self, timeslot: u64) -> Result<u64, ()> {
        for current_timeslot in timeslot.. {
            if let Some(block_ids) = self.cached_blocks_for_timeslot.remove(&current_timeslot) {
                for block_id in block_ids.iter() {
                    let cached_block = self.blocks.get(block_id).unwrap().clone();
                    if !self.validate_and_apply_cached_block(cached_block).await {
                        return Err(());
                    }
                }
            }

            if self.cached_blocks_for_timeslot.is_empty() {
                return Ok(current_timeslot);
            }

            if current_timeslot % TIMESLOTS_PER_EPOCH as u64 == 0 {
                // create the new epoch
                let current_epoch = self.epoch_tracker.advance_epoch().await;

                info!(
                    "Closed randomness for epoch {} during apply cached blocks",
                    current_epoch - CHALLENGE_LOOKBACK_EPOCHS
                );

                info!("Creating a new empty epoch for epoch {}", current_epoch);
            }
        }

        Ok(timeslot)
    }

    /// start the timer after syncing the ledger
    pub async fn start_timer_from_genesis_time(
        &mut self,
        timer_to_farmer_tx: Sender<FarmerMessage>,
        elapsed_timeslots: u64,
        is_farming: bool,
    ) {
        info!("Starting the timer from genesis time");

        self.timer_is_running = true;

        async_std::task::spawn(timer::run(
            timer_to_farmer_tx,
            self.epoch_tracker.clone(),
            elapsed_timeslots,
            is_farming,
            self.genesis_timestamp,
        ));
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
