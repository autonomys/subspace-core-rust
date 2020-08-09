#![allow(dead_code)]

use super::*;
use async_std::sync::Sender;
use ed25519_dalek::PublicKey;
use ed25519_dalek::Signature;
use log::*;
use manager::ProtocolMessage;
use network::NodeType;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Display;

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
    pub challenge: [u8; 32],
    pub tag: [u8; 32],
    pub public_key: [u8; 32],
    pub signature: Vec<u8>,
    pub tx_payload: Vec<u8>,
}

impl Block {
    pub fn new(
        timestamp: u128,
        delay: u32,
        challenge: [u8; 32],
        parent_id: [u8; 32],
        tag: [u8; 32],
        public_key: [u8; 32],
        signature: Vec<u8>,
        tx_payload: Vec<u8>,
    ) -> Block {
        Block {
            parent_id,
            challenge,
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
    pub proof_index: u64,
}

impl Proof {
    pub fn new(
        encoding: Piece,
        merkle_proof: Vec<u8>,
        piece_index: u64,
        proof_index: u64,
    ) -> Proof {
        Proof {
            encoding: encoding.to_vec(),
            merkle_proof,
            piece_index,
            proof_index,
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
            proof_index: u64,
        }

        let id = hex::encode(self.get_id());

        let pretty_proof = PrettyProof {
            encoding: hex::encode(self.encoding.clone()),
            merkle_proof: hex::encode(self.merkle_proof.clone()),
            piece_index: self.piece_index,
            proof_index: self.proof_index,
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

        if self.proof.proof_index != utils::modulo(&self.block.challenge, PIECE_COUNT) as u64 {
            warn!("Invalid full block, piece index does not match challenge and piece size");
            return false;
        }

        // validate the tag
        let local_tag = crypto::create_hmac(&self.proof.encoding, &self.block.challenge);
        if local_tag.cmp(&self.block.tag) != Ordering::Equal {
            warn!("Invalid full block, tag is invalid");
            return false;
        }

        // validate the merkle proof
        if !crypto::validate_merkle_proof(
            self.proof.proof_index as usize,
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

#[derive(PartialEq, Clone, Debug)]
pub enum BlockState {
    New,       // brand new block, generated locally or received via gossip
    Arrived,   // deadline has arrived
    Confirmed, // applied to the ledger w.h.p.
    Stray,     // received over the network, cannot find parent
}

impl Display for BlockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::New => "New",
                Self::Arrived => "Arrived",
                Self::Confirmed => "Confirmed",
                Self::Stray => "Stray",
            }
        )
    }
}

#[derive(PartialEq, Clone, Debug)]
pub struct MetaBlock {
    pub id: [u8; 32],            // hash of the block
    pub block: Block,            // the block itself
    pub state: BlockState,       // the state of the block
    pub children: Vec<[u8; 32]>, // child blocks, will grow quickly then be pruned down to one as confirmed
}

pub struct Ledger {
    pub metablocks: HashMap<[u8; 32], MetaBlock>,
    pub confirmed_blocks_by_index: HashMap<u32, [u8; 32]>,
    pub pending_children_for_parent: HashMap<[u8; 32], Vec<[u8; 32]>>,

    recent_blocks_by_id: HashMap<[u8; 32], Block>, // recently arrived blocks that have not been confirmed
    recent_children_by_parent_id: HashMap<[u8; 32], Vec<[u8; 32]>>,
    applied_blocks_by_id: HashMap<[u8; 32], Block>, // all applied blocks, stored by hash
    pending_blocks_by_id: HashMap<[u8; 32], FullBlock>, // all pending blocks (unseen parent) by hash

    balances: HashMap<[u8; 32], usize>, // the current balance of all accounts

    pub node_type: NodeType,
    pub height: u32,          // current block height
    pub quality: u32,         // aggregate quality for this chain
    pub merkle_root: Vec<u8>, // only inlcuded for test ledger
    pub genesis_piece_hash: [u8; 32],
    pub latest_block_hash: [u8; 32],
    pub quality_threshold: u8, // current quality target
    pub sloth: sloth::Sloth,   // a sloth instance for decoding (verifying) blocks
}

impl Ledger {
    pub fn new(merkle_root: Vec<u8>, genesis_piece_hash: [u8; 32], node_type: NodeType) -> Ledger {
        // init sloth
        let prime_size = PRIME_SIZE_BITS;
        let sloth = sloth::Sloth::init(prime_size);

        Ledger {
            metablocks: HashMap::new(),
            confirmed_blocks_by_index: HashMap::new(),
            pending_children_for_parent: HashMap::new(),

            recent_blocks_by_id: HashMap::new(),
            recent_children_by_parent_id: HashMap::new(),
            applied_blocks_by_id: HashMap::new(),
            pending_blocks_by_id: HashMap::new(),
            balances: HashMap::new(),
            node_type,
            height: 0,
            quality: 0,
            merkle_root,
            genesis_piece_hash,
            latest_block_hash: genesis_piece_hash,
            quality_threshold: INITIAL_QUALITY_THRESHOLD,
            sloth,
        }
    }

    pub fn track_block(&mut self, block: &Block, state: BlockState) {
        self.metablocks.insert(
            block.get_id(),
            MetaBlock {
                id: block.get_id(),
                block: block.clone(),
                state: state.clone(),
                children: Vec::new(),
            },
        );
        info!(
            "Created block: {} with {} state",
            hex::encode(&block.get_id()[0..8]),
            state
        );
    }

    /// Retrieve the first block at a given index (block height)
    pub fn get_block_by_index(&self, index: u32) -> Option<Block> {
        self.confirmed_blocks_by_index.get(&index).map(|block_id| {
            self.metablocks
                .get(&block_id.clone())
                .expect("Block index and blocks map have gotten out of sync!")
                .clone()
                .block
        })
    }

    // recursively remove all recent descendants for a set of siblings
    pub fn prune_branches(&mut self, siblings: Vec<[u8; 32]>) {
        siblings.iter().for_each(|sibling| {
            warn!("Pruning block: {}", hex::encode(&sibling[0..8]),);
            match self.metablocks.remove(sibling) {
                Some(block) => self.prune_branches(block.children),
                None => {}
            };
        });
    }

    /// On block arrival add it to the recent block pool and check if it results in a confirmation
    pub async fn apply_arrived_block(
        &mut self,
        block_id: &[u8; 32],
        sender: &Sender<ProtocolMessage>,
    ) -> Option<MetaBlock> {
        let block = match self.metablocks.get_mut(block_id) {
            Some(block) => block,
            None => return None,
        };
        block.state = BlockState::Arrived;
        let block = block.clone();

        // look for parent in recent blocks
        match self.metablocks.get_mut(&block.block.parent_id) {
            Some(first_parent_block) => {
                // solve on top of the block

                if self.node_type == NodeType::Farmer || self.node_type == NodeType::Gateway {
                    sender
                        .send(ProtocolMessage::BlockChallenge {
                            parent_id: block.id,
                            challenge: block.block.tag,
                            base_time: block.block.timestamp,
                        })
                        .await;

                    info!("Sent new block to solver");
                }

                // apply to pending ledger state and check for confimrations
                match first_parent_block.state {
                    BlockState::Arrived => {
                        // info!(
                        //     "Adding recent block {} with recent parent {}",
                        //     hex::encode(&block_id[0..8]),
                        //     hex::encode(&block.block.parent_id[0..8])
                        // );

                        // add a pointer in its parent
                        first_parent_block.children.push(*block_id);

                        let mut parent_block = first_parent_block.clone();
                        let mut branch_depth = 1;

                        // count recent blocks back
                        while let Some(next_parent_block) =
                            self.metablocks.get(&parent_block.block.parent_id)
                        {
                            // stop at the first confirmed block
                            if next_parent_block.state == BlockState::Confirmed {
                                break;
                            }

                            branch_depth += 1;
                            parent_block = next_parent_block.clone();
                        }

                        if branch_depth >= CONFIRMATION_DEPTH {
                            // apply the last parent and remove from recent blocks

                            // prune all other subtrees
                            let siblings = self
                                .metablocks
                                .get_mut(&parent_block.block.parent_id)
                                .unwrap()
                                .children
                                .drain_filter(|child| child != &mut parent_block.id)
                                .collect();

                            self.prune_branches(siblings);

                            // apply the new confirmed block
                            return self.confirm_block(&parent_block.id);
                        }

                        return Some(block.clone());
                    }
                    BlockState::Confirmed => {
                        // special case that only occurs when the second block is applied to the genesis block
                        // as there are no uncofirmed blocks yet
                        // might also occur if a block arrives immediately after the parent is confirmed
                        // no need to check for confirmations since the branch is only length one

                        // info!(
                        //     "Adding recent block {} with confirmed parent {}",
                        //     hex::encode(&block_id[0..8]),
                        //     hex::encode(&block.block.parent_id[0..8])
                        // );

                        if block.block.parent_id == self.latest_block_hash {
                            first_parent_block.children.push(block.id);
                            return Some(block.clone());
                        } else {
                            warn!("Recent block is late, its parent has already been confirmed!")
                        }
                    }
                    BlockState::New => {}
                    BlockState::Stray => {}
                }
            }
            None => {
                warn!(
                    "Do not have parent {} for recent block {}, dropping!",
                    hex::encode(&block_id[0..8]),
                    hex::encode(&block.block.parent_id[0..8])
                );
            }
        }

        // drop the block if it was not applied
        self.metablocks.remove(block_id);
        None

        // what about recent gossip for a node who is syncing?
    }

    /// Apply a new block to the ledger by id (hash)
    pub fn confirm_block(&mut self, block_id: &[u8; 32]) -> Option<MetaBlock> {
        let block = self.metablocks.get_mut(block_id).unwrap();
        block.state = BlockState::Confirmed;
        self.latest_block_hash = *block_id;

        // update height
        self.height += 1;

        // update quality
        self.quality += block.block.get_quality() as u32;

        // update balances, get or add account
        self.balances
            .entry(*block_id)
            .and_modify(|balance| *balance += block.block.reward as usize)
            .or_insert(block.block.reward as usize);

        // Adds a pointer to this block id for the given index in the ledger
        self.confirmed_blocks_by_index
            .insert(self.height - 1, *block_id);

        info!("Confirmed block: {}", hex::encode(&block_id[0..8]));

        Some(block.clone())
    }

    pub fn apply_genesis_block(&mut self, block: &Block) {
        let block_id = block.get_id();
        self.metablocks.insert(
            block_id,
            MetaBlock {
                id: block_id,
                block: block.clone(),
                state: BlockState::Confirmed,
                children: Vec::new(),
            },
        );

        self.latest_block_hash = block_id;

        // update height
        self.height += 1;

        // update quality
        self.quality += block.get_quality() as u32;

        // Adds a pointer to this block id for the given index in the ledger
        self.confirmed_blocks_by_index
            .insert(self.height - 1, block_id);
    }

    /// Apply a pending block that is cached to the ledger, recursively checking to see if it is the parent of some other pending block. Ledger should be fully synced when complete.
    // pub fn apply_pending_children(&mut self, parent_block_id: [u8; 32]) -> ([u8; 32], u128) {
    //     loop {
    //         match self.pending_parents_by_id.get(&parent_block_id) {
    //             Some(pending_child_id) => {
    //                 match self.pending_blocks_by_id.get(pending_child_id) {
    //                     Some(pending_full_block) => {
    //                         info!(
    //                             "Got pending child full block with id: {}",
    //                             hex::encode(&pending_full_block.block.get_id()[0..8])
    //                         );
    //                         // ensure the block is not in the ledger already
    //                         if self.is_block_applied(&pending_full_block.block.get_id()) {
    //                             panic!("Logic error, attempting to apply a pending block that is already in the ledger");
    //                         }
    //                         // ensure the parent is applied
    //                         if !self.is_block_applied(&pending_full_block.block.parent_id) {
    //                             panic!("Logic error, attempting to apply a pending block whose parent is not applied");
    //                         }
    //                         // TODO: use correct quality threshold (delay actually)
    //                         // if full_block.block.get_quality() < self.quality_threshold {
    //                         //     panic!("Logic error, cached full block has insufficient quality");
    //                         // }
    //                         let pending_block_copy = pending_full_block.block.clone();
    //                         // TODO: wait for arrival of each block before moving ahead

    //                         // remove from pending blocks and pending parents
    //                         self.pending_blocks_by_id
    //                             .remove(&pending_block_copy.get_id());
    //                         // self.pending_parents_by_id
    //                         //     .remove(&pending_block_copy.parent_id);
    //                         // add the block by id
    //                         // match self.confirm_block(&pending_block_copy.get_id()) {
    //                         //     BlockStatus::Confirmed => {
    //                         //         // either continue (call recursive) or solve
    //                         //         info!("Successfully applied pending block!");
    //                         //         parent_block_id = pending_block_copy.get_id();
    //                         //         if !self.is_pending_parent(&parent_block_id) {
    //                         //             return (parent_block_id, pending_block_copy.timestamp);
    //                         //         }
    //                         //     }
    //                         //     BlockStatus::Pending => {
    //                         //         panic!("Logic error, the block is already pending!");
    //                         //     }
    //                         //     BlockStatus::Invalid => {
    //                         //         panic!("Logic error, pending block should not be invalid!");
    //                         //     }
    //                         // }
    //                     }
    //                     None => {
    //                         panic!(
    //                             "Pending blocks are out of sync, cannot retrieve full pending block"
    //                         );
    //                     }
    //                 }
    //             }
    //             None => {
    //                 panic!("Pending blocks are out of sync, cannot retrieve parent reference");
    //             }
    //         }
    //     }
    // }

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
        let proof = Proof::new(encoding, merkle_proofs[17].clone(), 17u64, 245u64);
        let proof_id = proof.get_id();
        let proof_vec = proof.to_bytes();
        let proof_copy = Proof::from_bytes(&proof_vec).unwrap();
        let proof_copy_id = proof_copy.get_id();
        proof.print();
        assert_eq!(proof_id, proof_copy_id);
    }
}
