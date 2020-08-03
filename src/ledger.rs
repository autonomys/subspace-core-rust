#![allow(dead_code)]

use super::*;
use ed25519_dalek::PublicKey;
use ed25519_dalek::Signature;
use log::*;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;

/* ToDo
 * ----
 * Figure our right pattern for ensuring the deadline has passed before applying cached blocks
 * Ensure nodes who receive blocks via gossip/sync do not forward & apply them too early (eager attacks)
 * Likewise ensure nodes do not recirculate long delay blocks that could flood the network (DoS attacks)
 * Commits to the ledger should be atmoic (if we fail part way through)
 * Quality/Delay threshold checks and scoring should use the difficulty target for that time
 *
 * Why does consensus occasionally stall?
 * First, the channel buffers are overflowing -> use mpsc::unbounded?
 * Second, the disk reads are overflowing
 *
 * Degree of Simulation (DoS): How may solutions I present for each challenge
 * Confirmation Depth (CD): How deep a potential block may be before we confirm its parent and prune
 * Solve Complexity (SC): The number of disk reads per challenge (equal to my replication factor)
 *
 * Complexity = SC x (DoS + 1) ^ CD
 *
 * Example: 1 GB Ledger / 1 TB Drive => 1,000 RF
 * CD = 4 Levels
 * DoS = 8 (secure)
 *
 * Complexity = 1,000 x 9^4
 * Complexity = 6.5 Million 4k reads per challenge
 *
 * 24 GB/sec read throughput
 *
 * 256 x 8^3 = 500 Mb/sec
 *
 * Refactoring
 * -----------
 * Store all blocks in the same hashmap with a wrapper for status
 * Statuses include confirmed, pending_arrival, pending_parent, pending_confirmation
 *
 * new local -> pending arrival
 * new gossip -> pending parent or pending arrival
 * arrives -> pending confirmation (or discarded)
 * confirmed -> (or discarded)
 *
 * new via sync -> check for arrival, check for parent
 *
 * Instead of passing blocks around in protocol messages, just pass their id
 *
 *
 * TESTING
 * -------
 * Piece count is always 256 for testing for the merkle tree
 * Plot size is configurable, but must be a multiple of 256
 * For each challenge, solver will check every 256th piece starting at index and return the top N
 * We want to start with 256 x 256 pieces
 * This mean for each challenge the expected quality should be below 2^32 / 2^8 -> 2^24
 *
 * SECURITY
 * --------
 * measure how far ahead a node can predict, based on its storage power
 * combine with simulation to see what advantage this provides
 * measure how much time this gives it for on-demand encoding
 *
 *
*/

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Block {
    pub reward: u32,
    pub timestamp: u128,
    pub delay: u32,
    pub parent_id: [u8; 32],
    pub tag: [u8; 32],
    pub public_key: [u8; 32],
    pub signature: Vec<u8>,
    pub tx_payload: Vec<u8>,
}

impl Block {
    pub fn new(
        timestamp: u128,
        delay: u32,
        parent_id: [u8; 32],
        tag: [u8; 32],
        public_key: [u8; 32],
        signature: Vec<u8>,
        tx_payload: Vec<u8>,
    ) -> Block {
        Block {
            parent_id,
            timestamp,
            delay,
            tag,
            public_key,
            signature,
            reward: 1,
            tx_payload,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            warn!("Failed to deserialize Block: {}", error);
            ()
        })
    }

    pub fn get_id(&self) -> [u8; 32] {
        crypto::digest_sha_256(&self.to_bytes())
    }

    // TODO: base on difficulty threshold
    pub fn get_quality(&self) -> u8 {
        utils::measure_quality(&self.tag)
    }

    // TODO: Should probably be a `validate()` method that returns `Result<(), BlockValidationError>`
    //  or `BlockValidationResult` instead of printing to the stdout
    pub fn is_valid(&self) -> bool {
        // verify the signature
        let public_key = PublicKey::from_bytes(&self.public_key).unwrap();
        let signature = Signature::from_bytes(&self.signature).unwrap();

        if public_key.verify_strict(&self.tag, &signature).is_err() {
            warn!("Invalid block, signature is invalid");
            return false;
        }

        true
    }

    pub fn print(&self) {
        #[derive(Debug)]
        pub struct PrettyBlock {
            reward: u32,
            timestamp: u128,
            parent_id: String,
            tag: String,
            public_key: String,
            signature: String,
            // tx_payload: String,
        }

        let id = hex::encode(self.get_id());

        let pretty_block = PrettyBlock {
            reward: self.reward,
            timestamp: self.timestamp,
            parent_id: hex::encode(self.parent_id),
            tag: hex::encode(self.tag),
            public_key: hex::encode(self.public_key),
            signature: hex::encode(self.signature.clone()),
            // tx_payload: hex::encode(self.tx_payload.clone()),
        };

        // Should probably return data structure, but not print to the stdout
        info!("Block with id: {}\n{:#?}", id, pretty_block);
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Proof {
    pub encoding: Vec<u8>,
    pub merkle_proof: Vec<u8>,
    pub piece_index: u64,
}

impl Proof {
    pub fn new(encoding: Piece, merkle_proof: Vec<u8>, piece_index: u64) -> Proof {
        Proof {
            encoding: encoding.to_vec(),
            merkle_proof,
            piece_index,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            warn!("Failed to deserialize proof: {}", error);
            ()
        })
    }

    pub fn get_id(&self) -> [u8; 32] {
        crypto::digest_sha_256(&self.to_bytes())
    }

    pub fn print(&self) {
        #[derive(Debug)]
        struct PrettyProof {
            encoding: String,
            merkle_proof: String,
            piece_index: u64,
        }

        let id = hex::encode(self.get_id());

        let pretty_proof = PrettyProof {
            encoding: hex::encode(self.encoding.clone()),
            merkle_proof: hex::encode(self.merkle_proof.clone()),
            piece_index: self.piece_index,
        };

        info!("Aux data with id: {}\n{:#?}", id, pretty_proof);
    }
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
pub struct FullBlock {
    pub block: Block,
    pub proof: Proof,
}

impl FullBlock {
    pub fn is_valid(
        &self,
        merkle_root: &[u8],
        genesis_piece_hash: &[u8; 32],
        sloth: &sloth::Sloth,
    ) -> bool {
        // ensure challenge index is correct

        let adjusted_proof_index = self.proof.piece_index % PIECE_COUNT as u64;
        let derived_proof_index = utils::modulo(&self.block.parent_id, PIECE_COUNT) as u64;

        if adjusted_proof_index != derived_proof_index {
            warn!("Adjusted is: {}", adjusted_proof_index);
            warn!("Derived is: {}", derived_proof_index);
            warn!("Invalid full block, piece index does not match challenge and piece size");
            return false;
        }

        // validate the tag
        let local_tag = crypto::create_hmac(&self.proof.encoding, &self.block.parent_id);
        if local_tag.cmp(&self.block.tag) != Ordering::Equal {
            warn!("Invalid full block, tag is invalid");
            return false;
        }

        // validate the merkle proof
        if !crypto::validate_merkle_proof(
            self.proof.piece_index as usize,
            &self.proof.merkle_proof,
            merkle_root,
        ) {
            warn!("Invalid full block, merkle proof is invalid");
            return false;
        }

        // validate the encoding
        let id = crypto::digest_sha_256(&self.block.public_key);
        let expanded_iv = crypto::expand_iv(id);
        let layers = ENCODING_LAYERS_TEST;
        let mut decoding = self.proof.encoding.clone();

        sloth.decode(&mut decoding[..], expanded_iv, layers);

        // subtract out the index when comparing to the genesis piece
        let index_bytes = utils::usize_to_bytes(self.proof.piece_index as usize);
        for i in 0..16 {
            decoding[i] = decoding[i] ^ index_bytes[i];
        }

        let decoding_hash = crypto::digest_sha_256(&decoding.to_vec());
        if genesis_piece_hash.cmp(&decoding_hash) != Ordering::Equal {
            warn!("Invalid full block, encoding is invalid");
            // utils::compare_bytes(&proof.encoding, &proof.encoding, &decoding);
            return false;
        }

        // verify the signature
        let public_key = PublicKey::from_bytes(&self.block.public_key).unwrap();
        let signature = Signature::from_bytes(&self.block.signature).unwrap();
        if public_key
            .verify_strict(&self.block.tag, &signature)
            .is_err()
        {
            warn!("Invalid full block, signature is invalid");
            return false;
        }

        true
    }
}

pub enum BlockStatus {
    // Arrived,   // tracked in recent blocks, but not included in the ledger
    Confirmed, // applied to the ledger, extending the current head legally
    Pending,   // parent is unknown, cached until parent is received
    Invalid,   // attempted to apply to the ledger but could not
}

pub struct Ledger {
    recent_blocks_by_id: HashMap<[u8; 32], Block>, // recently arrived blocks that have not been confirmed
    recent_children_by_parent_id: HashMap<[u8; 32], Vec<[u8; 32]>>,
    applied_blocks_by_id: HashMap<[u8; 32], Block>, // all applied blocks, stored by hash
    applied_block_id_by_index: HashMap<u32, [u8; 32]>, // hash for applied blocks, by block height
    balances: HashMap<[u8; 32], usize>,             // the current balance of all accounts
    pending_blocks_by_id: HashMap<[u8; 32], FullBlock>, // all pending blocks (unseen parent) by hash
    pending_parents_by_id: HashMap<[u8; 32], [u8; 32]>, // all parents we are expecting, with their child (which will be in pending blocks)
    pub height: u32,                                    // current block height
    pub quality: u32,                                   // aggregate quality for this chain
    pub merkle_root: Vec<u8>,                           // only inlcuded for test ledger
    pub genesis_piece_hash: [u8; 32],
    pub latest_block_hash: [u8; 32],
    pub quality_threshold: u8, // current quality target
    pub sloth: sloth::Sloth,   // a sloth instance for decoding (verifying) blocks
}

impl Ledger {
    pub fn new(merkle_root: Vec<u8>, genesis_piece_hash: [u8; 32]) -> Ledger {
        // init sloth
        let prime_size = PRIME_SIZE_BITS;
        let sloth = sloth::Sloth::init(prime_size);

        Ledger {
            recent_blocks_by_id: HashMap::new(),
            recent_children_by_parent_id: HashMap::new(),
            applied_blocks_by_id: HashMap::new(),
            applied_block_id_by_index: HashMap::new(),
            pending_blocks_by_id: HashMap::new(),
            pending_parents_by_id: HashMap::new(),
            balances: HashMap::new(),
            height: 0,
            quality: 0,
            merkle_root,
            genesis_piece_hash,
            latest_block_hash: genesis_piece_hash,
            quality_threshold: INITIAL_QUALITY_THRESHOLD,
            sloth,
        }
    }

    /// Check if you have applied a block to the ledger
    pub fn is_block_applied(&self, id: &[u8; 32]) -> bool {
        self.applied_blocks_by_id.contains_key(id)
    }

    /// Check if a block is the parent of some cached block.
    pub fn is_pending_parent(&self, block_id: &[u8; 32]) -> bool {
        self.pending_parents_by_id.contains_key(block_id)
    }

    /// Retrieve the first block at a given index (block height)
    pub fn get_block_by_index(&self, index: u32) -> Option<Block> {
        self.applied_block_id_by_index
            .get(&index)
            .and_then(|block_id| {
                Some(
                    self.applied_blocks_by_id
                        .get(&block_id.clone())
                        .expect("Block index and blocks map have gotten out of sync!")
                        .clone(),
                )
            })
    }

    /// Retrieve a block by id (hash)
    pub fn get_confirmed_block_by_id(&self, id: &[u8; 32]) -> Option<Block> {
        match self.applied_blocks_by_id.get(id) {
            Some(block) => Some(block.clone()),
            None => None,
        }
    }

    /// Retrieve a recent block by id (hash)
    pub fn get_recent_block_by_id(&self, id: &[u8; 32]) -> Option<Block> {
        match self.recent_blocks_by_id.get(id) {
            Some(block) => Some(block.clone()),
            None => None,
        }
    }

    // recursively remove all recent descendants for a set of siblings
    pub fn prune_branches(&mut self, siblings: Vec<[u8; 32]>) {
        for sibling in siblings.iter() {
            // remove the sibling from recent blocks
            self.recent_blocks_by_id.remove(sibling);

            match self.recent_children_by_parent_id.get(sibling) {
                Some(children) => {
                    // remove all children
                    #[allow(mutable_borrow_reservation_conflict)]
                    self.prune_branches(children.clone());
                }
                None => {}
            }

            // remove the pointers to its children
            self.recent_children_by_parent_id.remove(sibling);
            // warn!("Removing pruned child block");
        }
    }

    /// On block arrival add it to the recent block pool and check if it results in a confirmation
    pub fn apply_recent_block(&mut self, block: &Block) -> BlockStatus {
        // given a block_id of a recently arrived block

        if self.height == 0 {
            // genesis block, apply and confirm immediately
            info!("Applying the genesis block");
            return self.apply_block_by_id(block);
        } else {
            // normal block, find parent and handle

            // look for parent in recent blocks
            match self.get_recent_block_by_id(&block.parent_id) {
                Some(first_parent_block) => {
                    let block_id = block.get_id();

                    // info!("Applying a block with a recent parent");

                    // add to recent blocks
                    self.recent_blocks_by_id.insert(block_id, block.clone());

                    // get or create reference to parent, then add
                    self.recent_children_by_parent_id
                        .entry(block.parent_id)
                        .and_modify(|children| children.push(block_id))
                        .or_insert(vec![block_id]);

                    let mut parent_block = first_parent_block;
                    let mut branch_depth = 1;

                    // count recent blocks back
                    while let Some(next_parent_block) =
                        self.get_recent_block_by_id(&parent_block.parent_id)
                    {
                        branch_depth += 1;
                        parent_block = next_parent_block;
                    }

                    // info!(
                    //     "Branch depth is is: {}/{}",
                    //     branch_depth, CONFIRMATION_DEPTH
                    // );

                    if branch_depth >= CONFIRMATION_DEPTH {
                        // apply the last parent and remove from recent blocks
                        let confirmed_block_id = parent_block.get_id();
                        self.recent_blocks_by_id.remove(&confirmed_block_id);

                        // warn!("Confirming block, removing from recent and deleting child pointers");

                        // remove all siblings of oldest parent
                        let siblings: Vec<[u8; 32]> = self
                            .recent_children_by_parent_id
                            .get(&parent_block.parent_id)
                            .unwrap()
                            .clone()
                            .iter()
                            .filter(|child_id| **child_id != confirmed_block_id)
                            .map(|child_id| *child_id)
                            .collect();

                        self.prune_branches(siblings);

                        // stop tracking the last confirmed block
                        self.recent_children_by_parent_id
                            .remove(&parent_block.parent_id);

                        // apply the new confirmed block
                        return self.apply_block_by_id(&parent_block);
                    }

                    // TODO: Change this status to a better name
                    return BlockStatus::Confirmed;
                }
                None => {
                    // warn!(
                    //     "Could not find parent block in recent blocks, checking confirmed blocks"
                    // );
                }
            }

            // look for parent in confimred blocks
            match self.get_confirmed_block_by_id(&block.parent_id) {
                Some(_parent_block) => {
                    if block.parent_id == self.latest_block_hash {
                        let block_id = block.get_id();
                        // add to recent blocks
                        self.recent_blocks_by_id.insert(block_id, block.clone());

                        // get or create reference to parent, then add child
                        self.recent_children_by_parent_id
                            .entry(block.parent_id)
                            .and_modify(|children| children.push(block_id))
                            .or_insert(vec![block_id]);

                        // info!("Added recent block directly to confirmed block");

                        // TODO: Change this status to a better name
                        return BlockStatus::Confirmed;
                    } else {
                        // warn!("Recent block is late, its parent has already been confirmed!")
                    }
                }
                None => {
                    // warn!("Recent block could not be applied, could not find a parent!");
                }
            }

            BlockStatus::Invalid
        }

        // what about recent gossip for a node who is syncing?
    }

    /// Apply a new block to the ledger by id (hash)
    pub fn apply_block_by_id(&mut self, block: &Block) -> BlockStatus {
        let block_id = block.get_id();

        self.latest_block_hash = block_id;

        // update height
        self.height += 1;

        // update quality
        self.quality += block.get_quality() as u32;

        // update balances, get or add account
        self.balances
            .entry(block_id)
            .and_modify(|balance| *balance += block.reward as usize)
            .or_insert(block.reward as usize);

        self.applied_blocks_by_id.insert(block_id, block.clone());

        // Adds a pointer to this block id for the given index in the ledger
        self.applied_block_id_by_index
            .insert(self.height - 1, block_id);

        // info!("Added block with id: {}", hex::encode(&block_id[0..8]));

        BlockStatus::Confirmed
    }

    /// Cache a pending block and add parent to watch list. When parent is received, it will be applied to the ledger.
    pub fn cache_pending_block(&mut self, full_block: FullBlock) {
        let block_id = full_block.block.get_id();
        self.pending_parents_by_id
            .insert(full_block.block.parent_id, block_id.clone());
        self.pending_blocks_by_id.insert(block_id, full_block);
    }

    /// Apply a pending block that is cached to the ledger, recursively checking to see if it is the parent of some other pending block. Ledger should be fully synced when complete.
    pub fn apply_pending_children(&mut self, mut parent_block_id: [u8; 32]) -> ([u8; 32], u128) {
        loop {
            match self.pending_parents_by_id.get(&parent_block_id) {
                Some(pending_child_id) => {
                    match self.pending_blocks_by_id.get(pending_child_id) {
                        Some(pending_full_block) => {
                            info!(
                                "Got pending child full block with id: {}",
                                hex::encode(&pending_full_block.block.get_id()[0..8])
                            );
                            // ensure the block is not in the ledger already
                            if self.is_block_applied(&pending_full_block.block.get_id()) {
                                panic!("Logic error, attempting to apply a pending block that is already in the ledger");
                            }
                            // ensure the parent is applied
                            if !self.is_block_applied(&pending_full_block.block.parent_id) {
                                panic!("Logic error, attempting to apply a pending block whose parent is not applied");
                            }
                            // TODO: use correct quality threshold (delay actually)
                            // if full_block.block.get_quality() < self.quality_threshold {
                            //     panic!("Logic error, cached full block has insufficient quality");
                            // }
                            let pending_block_copy = pending_full_block.block.clone();
                            // TODO: wait for arrival of each block before moving ahead

                            // remove from pending blocks and pending parents
                            self.pending_blocks_by_id
                                .remove(&pending_block_copy.get_id());
                            self.pending_parents_by_id
                                .remove(&pending_block_copy.parent_id);
                            // add the block by id
                            match self.apply_block_by_id(&pending_block_copy) {
                                BlockStatus::Confirmed => {
                                    // either continue (call recursive) or solve
                                    info!("Successfully applied pending block!");
                                    parent_block_id = pending_block_copy.get_id();
                                    if !self.is_pending_parent(&parent_block_id) {
                                        return (parent_block_id, pending_block_copy.timestamp);
                                    }
                                }
                                BlockStatus::Pending => {
                                    panic!("Logic error, the block is already pending!");
                                }
                                BlockStatus::Invalid => {
                                    panic!("Logic error, pending block should not be invalid!");
                                }
                            }
                        }
                        None => {
                            panic!(
                                "Pending blocks are out of sync, cannot retrieve full pending block"
                            );
                        }
                    }
                }
                None => {
                    panic!("Pending blocks are out of sync, cannot retrieve parent reference");
                }
            }
        }
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

    pub fn get_block_height(&self) -> usize {
        // TODO: maybe this should be cached locally?
        self.applied_blocks_by_id.len()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn block() {
        let tx_payload = crypto::generate_random_piece().to_vec();
        let block = Block::new(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_millis(),
            300u32,
            crypto::random_bytes_32(),
            crypto::random_bytes_32(),
            crypto::random_bytes_32(),
            [0u8; 64].to_vec(),
            tx_payload,
        );
        let block_id = block.get_id();
        let block_vec = block.to_bytes();
        let block_copy = Block::from_bytes(&block_vec).unwrap();
        let block_copy_id = block_copy.get_id();
        block.print();
        assert_eq!(block_id, block_copy_id);
    }

    #[test]
    fn auxillary_data() {
        let encoding = crypto::generate_random_piece();
        let (merkle_proofs, _) = crypto::build_merkle_tree();
        let proof = Proof::new(encoding, merkle_proofs[17].clone(), 17u64);
        let proof_id = proof.get_id();
        let proof_vec = proof.to_bytes();
        let proof_copy = Proof::from_bytes(&proof_vec).unwrap();
        let proof_copy_id = proof_copy.get_id();
        proof.print();
        assert_eq!(proof_id, proof_copy_id);
    }
}
