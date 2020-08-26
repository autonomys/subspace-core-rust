#![allow(dead_code)]

use super::*;
use async_std::sync::Sender;
use ed25519_dalek::PublicKey;
use ed25519_dalek::Signature;
use log::*;
use manager::ProtocolMessage;
use network::NodeType;
use serde::{Deserialize, Serialize};
use solver::Solution;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::fmt;
use std::fmt::Display;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/* ToDo
 * ----
 *
 * Sync and apply blocks
 * Sync and farm blocks
 *
 * Make difficulty self-adjusting
 * Track chain quality
 * Track parent links to order blocks and transactions
 *
 * Node should not gossip blocks that are too far into the future
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

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Block {
    pub proof: Proof,
    pub content: Content,
    pub data: Option<Data>,
}

impl Block {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            warn!("Failed to deserialize Block: {}", error);
        })
    }

    pub fn get_id(&self) -> BlockId {
        let mut pruned_block = self.clone();
        pruned_block.prune();
        crypto::digest_sha_256(&pruned_block.to_bytes())
    }

    pub fn is_valid(
        &self,
        merkle_root: &[u8],
        genesis_piece_hash: &[u8; 32],
        epoch_randomness: &[u8; 32],
        slot_challenge: &[u8; 32],
        sloth: &sloth::Sloth,
    ) -> bool {
        // ensure we have the auxillary data
        if self.data.is_none() {
            warn!("Invalid block, missing auxillary data!");
            return false;
        }

        // does the content reference the correct proof?
        if self.content.proof_id != self.proof.get_id() {
            warn!("Invalid block, content and proof do not match!");
            return false;
        }

        let public_key = PublicKey::from_bytes(&self.proof.public_key).unwrap();
        let proof_signature = Signature::from_bytes(&self.content.proof_signature).unwrap();

        // is the proof signature valid?
        if public_key
            .verify_strict(&self.proof.get_id(), &proof_signature)
            .is_err()
        {
            warn!("Invalid block, proof signature is invalid!");
            return false;
        }

        let content_signature = Signature::from_bytes(&self.content.signature).unwrap();
        let mut content = self.content.clone();
        content.signature.clear();

        // is the content signature valid?
        if public_key
            .verify_strict(&content.get_id(), &content_signature)
            .is_err()
        {
            warn!("Invalid block, content signature is invalid!");
            return false;
        }

        // is the epoch challenge correct?
        if epoch_randomness != &self.proof.randomness {
            warn!("Invalid block, epoch randomness is incorrect!");
            return false;
        }

        // is the tag within range of the slot challenge?
        // let slot_seed = [
        //     &epoch_randomness[..],
        //     &self.proof.timeslot.to_le_bytes()[..],
        // ]
        // .concat();
        // let slot_challenge = crypto::digest_sha_256_simple(&slot_seed);
        let target = u64::from_be_bytes(slot_challenge[0..8].try_into().unwrap());
        let (distance, _) = target.overflowing_sub(self.proof.tag);

        if distance > SOLUTION_RANGE {
            warn!("Invalid block, solution does not meet the difficulty target!");
            return false;
        }

        // is the tag valid for the encoding and salt?
        let tag_hash = crypto::create_hmac(
            &self.data.as_ref().unwrap().encoding,
            &self.proof.nonce.to_le_bytes(),
        );
        let derived_tag = u64::from_le_bytes(tag_hash[0..8].try_into().unwrap());
        if derived_tag.cmp(&self.proof.tag) != Ordering::Equal {
            warn!("Invalid block, tag is invalid");
            return false;
        }

        // TODO: is the timestamp within drift?

        // is the merkle proof correct?
        if !crypto::validate_merkle_proof(
            self.proof.piece_index as usize,
            &self.data.as_ref().unwrap().merkle_proof,
            merkle_root,
        ) {
            warn!("Invalid block, merkle proof is invalid!");
            return false;
        }

        // is the encoding valid for the public key and index?
        let id = crypto::digest_sha_256(&self.proof.public_key);
        let expanded_iv = crypto::expand_iv(id);
        let layers = ENCODING_LAYERS_TEST;
        let mut decoding = self.data.as_ref().unwrap().encoding.clone();

        sloth.decode(decoding.as_mut(), expanded_iv, layers);

        // subtract out the index when comparing to the genesis piece
        let index_bytes = utils::usize_to_bytes(self.proof.piece_index as usize);
        for i in 0..16 {
            decoding[i] ^= index_bytes[i];
        }

        let decoding_hash = crypto::digest_sha_256(&decoding);
        if genesis_piece_hash.cmp(&decoding_hash) != Ordering::Equal {
            warn!("Invalid block, encoding is invalid");
            // utils::compare_bytes(&proof.encoding, &proof.encoding, &decoding);
            return false;
        }

        true
    }

    pub fn prune(&mut self) {
        self.data = None;
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Proof {
    pub randomness: ProofId,  // epoch challenge
    pub epoch: u64,           // epoch index
    pub timeslot: u64,        // time slot
    pub public_key: [u8; 32], // farmers public key
    pub tag: Tag,             // hmac of encoding with a nonce
    pub nonce: u128,          // nonce for salting the tag
    pub piece_index: u64,     // index of piece for encoding
}

impl Proof {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            warn!("Failed to deserialize Proof: {}", error);
        })
    }

    pub fn get_id(&self) -> ProofId {
        crypto::digest_sha_256(&self.to_bytes())
    }

    pub fn is_valid() {
        // is epoch challenge correct

        // is the slot challenge correct (from epoch challenge)

        // is the encoding correct

        // is the tag correct
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Content {
    pub parent_ids: Vec<ContentId>, // ids of all parent blocks not yet seen
    pub proof_id: ProofId,          // id of matching proof
    pub proof_signature: Vec<u8>,   // signature of the proof with same public key
    pub timestamp: u128,            // when this block was created (from Nodes local view)
    // TODO: Should be a vec of TX IDs
    pub tx_ids: Vec<u8>, // ids of all unseen transactions seen by this block
    // TODO: account for farmers who sign the same proof with two different contents
    pub signature: Vec<u8>, // signature of the content with same public key
}

impl Content {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            warn!("Failed to deserialize Content: {}", error);
        })
    }

    pub fn get_id(&self) -> ContentId {
        crypto::digest_sha_256(&self.to_bytes())
    }

    pub fn is_valid() {
        // is proof signature valid (requires public key)

        // is timestamp valid

        // is signature valid (requires public key)
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Data {
    pub encoding: Vec<u8>,     // the encoding of the piece with public key
    pub merkle_proof: Vec<u8>, // merkle proof showing piece is in the ledger
}

impl Data {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            warn!("Failed to deserialize Data: {}", error);
        })
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
    pub id: BlockId,            // hash of the block
    pub block: Block,           // the block itself
    pub state: BlockState,      // the state of the block
    pub children: Vec<BlockId>, // child blocks, will grow quickly then be pruned down to one as confirmed
}

// TODO: Make some type of epoch tracker structure

pub struct Ledger {
    pub metablocks: HashMap<BlockId, MetaBlock>,
    pub epochs: timer::EpochTracker,
    pub current_epoch: u64,
    pub current_timeslot: u64,
    pub confirmed_blocks_by_timeslot: HashMap<u64, Vec<BlockId>>,
    // TODO: add ordered confirmed_blocks_by_block_height
    pub pending_children_for_parent: HashMap<BlockId, Vec<BlockId>>,
    balances: HashMap<[u8; 32], usize>, // the current balance of all accounts
    pub unseen_block_ids: HashSet<BlockId>,
    pub genesis_timestamp: u128,

    pub node_type: NodeType,
    pub height: u32,          // current block height
    pub quality: u32,         // aggregate quality for this chain
    pub merkle_root: Vec<u8>, // only inlcuded for test ledger
    pub genesis_piece_hash: [u8; 32],
    pub latest_block_hash: BlockId,
    pub quality_threshold: u8, // current quality target
    pub sloth: sloth::Sloth,   // a sloth instance for decoding (verifying) blocks
    pub keys: ed25519_dalek::Keypair,
    pub tx_payload: Vec<u8>,
    pub merkle_proofs: Vec<Vec<u8>>,
}

impl Ledger {
    pub fn new(
        merkle_root: Vec<u8>,
        genesis_piece_hash: [u8; 32],
        node_type: NodeType,
        keys: ed25519_dalek::Keypair,
        tx_payload: Vec<u8>,
        merkle_proofs: Vec<Vec<u8>>,
        epochs: timer::EpochTracker,
    ) -> Ledger {
        // init sloth
        let prime_size = PRIME_SIZE_BITS;
        let sloth = sloth::Sloth::init(prime_size);

        Ledger {
            metablocks: HashMap::new(),
            epochs,
            current_epoch: 0,
            current_timeslot: 0,
            confirmed_blocks_by_timeslot: HashMap::new(),
            pending_children_for_parent: HashMap::new(),
            unseen_block_ids: HashSet::new(),
            genesis_timestamp: 0,
            balances: HashMap::new(),
            node_type,
            height: 0,
            quality: 0,
            merkle_root,
            genesis_piece_hash,
            latest_block_hash: genesis_piece_hash,
            quality_threshold: INITIAL_QUALITY_THRESHOLD,
            sloth,
            keys,
            tx_payload,
            merkle_proofs,
        }
    }

    /// Retrieve all blocks for a timeslot, return an empty vec if no blocks
    pub fn get_blocks_by_timeslot(&self, timeslot: u64) -> Vec<Block> {
        match self.confirmed_blocks_by_timeslot.get(&timeslot) {
            Some(block_ids) => {
                let mut blocks: Vec<Block> = Vec::new();
                for block_id in block_ids.iter() {
                    blocks.push(self.metablocks.get(block_id).unwrap().block.clone());
                }
                return blocks;
            }
            None => return vec![],
        };
    }

    pub async fn init_from_genesis(&mut self) {
        self.genesis_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        let mut timestamp = self.genesis_timestamp;
        let mut parent_id: BlockId = [0u8; 32];

        for epoch_index in 0..CHALLENGE_LOOKBACK {
            let mut epoch = timer::Epoch::new(epoch_index as u64);

            for _ in 0..TIMESLOTS_PER_EPOCH {
                let proof = Proof {
                    randomness: self.genesis_piece_hash,
                    epoch: self.current_epoch,
                    timeslot: self.current_timeslot,
                    public_key: self.keys.public.to_bytes(),
                    tag: 0,
                    nonce: 0,
                    piece_index: 0,
                };

                let mut content = Content {
                    parent_ids: vec![parent_id],
                    proof_id: proof.get_id(),
                    proof_signature: self.keys.sign(&proof.get_id()).to_bytes().to_vec(),
                    timestamp: timestamp,
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

                self.apply_genesis_block(&block);
                parent_id = block.get_id();

                self.current_timeslot += 1;
                epoch.slots.insert(self.current_timeslot, vec![parent_id]);

                info!(
                    "Applied a genesis block to ledger with id {}",
                    hex::encode(&parent_id)
                );
                let time_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_millis();

                timestamp += TIMESLOT_DURATION;
                async_std::task::sleep(Duration::from_millis((timestamp - time_now) as u64)).await;
            }

            epoch.close();
            self.epochs.lock().await.insert(self.current_epoch, epoch);

            info!("Updated randomness for epoch: {}", self.current_epoch);
            self.current_epoch += 1;
        }

        // init the first epoch
        let first_random_epoch = timer::Epoch::new(self.current_epoch);
        self.epochs
            .lock()
            .await
            .insert(self.current_epoch, first_random_epoch);

        self.unseen_block_ids.insert(parent_id);
    }

    pub fn apply_genesis_block(&mut self, block: &Block) {
        let block_id = block.get_id();
        let mut pruned_block = block.clone();
        pruned_block.prune();
        self.metablocks.insert(
            block_id,
            MetaBlock {
                id: block_id,
                block: pruned_block,
                state: BlockState::Confirmed,
                children: Vec::new(),
            },
        );

        // update height
        self.height += 1;

        // Adds a pointer to this block id for the given timeslot in the ledger
        self.confirmed_blocks_by_timeslot
            .entry(self.current_timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);
    }

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
                    .as_millis();
                // must get parents from the chain
                // TODO: only get parents that are not at the same level
                // TODO: create an empty vec then do memswap between unseen parent and unseen block ids
                let unseen_parents: Vec<BlockId> = self.unseen_block_ids.drain().collect();
                let mut content = Content {
                    parent_ids: unseen_parents,
                    proof_id: proof.get_id(),
                    proof_signature: self.keys.sign(&proof.get_id()).to_bytes().to_vec(),
                    timestamp: timestamp,
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

                let block_id = block.get_id();
                self.unseen_block_ids.insert(block_id);
                // apply the block to the ledger

                let mut pruned_block = block.clone();
                pruned_block.prune();
                self.metablocks.insert(
                    block_id,
                    MetaBlock {
                        id: block_id,
                        block: pruned_block,
                        state: BlockState::Confirmed,
                        children: Vec::new(),
                    },
                );
                // TODO: update chain quality
                // update balances, get or add account
                self.balances
                    .entry(crypto::digest_sha_256(&block.proof.public_key))
                    .and_modify(|balance| *balance += 1)
                    .or_insert(1);
                // Adds a pointer to this block id for the given timelsot in the ledger
                self.confirmed_blocks_by_timeslot
                    .entry(self.current_timeslot)
                    .and_modify(|block_ids| block_ids.push(block_id))
                    .or_insert(vec![block_id]);

                // TODO: collect all blocks for a slot, then order blocks, then order tx

                // update the epoch for this block
                // TODO: Make convenience method on epoch that returns a result
                let mut new_timeslot = true;
                self.epochs
                    .lock()
                    .await
                    .entry(block.proof.epoch)
                    .and_modify(|epoch| {
                        epoch
                            .slots
                            .entry(block.proof.timeslot)
                            .and_modify(|timeslot| {
                                timeslot.push(block_id);
                                new_timeslot = false;
                            })
                            .or_insert(vec![block_id]);
                    });

                // if we are on the last timeslot, advance the epoch
                if (self.current_timeslot + 1) % TIMESLOTS_PER_EPOCH == 0 {
                    self.current_epoch += 1;
                }

                if new_timeslot {
                    self.current_timeslot += 1;
                }

                // info!("Applied block to ledger at slot: {}", self.current_timeslot);
                return Some(block);
            }
            None => {
                if (self.current_timeslot + 1) % TIMESLOTS_PER_EPOCH == 0 {
                    self.current_epoch += 1;
                }

                self.current_timeslot += 1;
                return None;
            }
        };
    }

    pub async fn validate_and_apply_remote_block(&mut self, block: Block) -> bool {
        // get correct randomness for this block
        let epoch = self
            .epochs
            .lock()
            .await
            .get(&(block.proof.epoch - CHALLENGE_LOOKBACK))
            .unwrap()
            .clone();

        // check if the block is valid
        if !block.is_valid(
            &self.merkle_root,
            &self.genesis_piece_hash,
            &epoch.randomness,
            &epoch.challenges[block.proof.timeslot as usize],
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

        self.metablocks.insert(
            block_id,
            MetaBlock {
                id: block_id,
                block: pruned_block,
                state: BlockState::Confirmed,
                children: Vec::new(),
            },
        );
        // TODO: update chain quality
        // update balances, get or add account
        self.balances
            .entry(crypto::digest_sha_256(&block.proof.public_key))
            .and_modify(|balance| *balance += 1)
            .or_insert(1);
        // Adds a pointer to this block id for the given timelsot in the ledger
        self.confirmed_blocks_by_timeslot
            .entry(self.current_timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);

        // TODO: collect all blocks for a slot, then order blocks, then order tx

        // update the epoch for this block
        // TODO: Make convenience method on epoch that returns a result
        let mut new_timeslot = true;
        self.epochs
            .lock()
            .await
            .entry(block.proof.epoch)
            .and_modify(|epoch| {
                epoch
                    .slots
                    .entry(block.proof.timeslot)
                    .and_modify(|timeslot| {
                        timeslot.push(block_id);
                        new_timeslot = false;
                    })
                    .or_insert(vec![block_id]);
            });

        // if we are on the last timeslot, close the epoch
        if (self.current_timeslot + 1) % TIMESLOTS_PER_EPOCH == 0 {
            self.current_epoch += 1;
        }

        if new_timeslot {
            self.current_timeslot += 1;
        }

        true
    }

    pub async fn apply_block_from_sync(&mut self, block: Block) {
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
            .entry(self.current_timeslot)
            .and_modify(|block_ids| block_ids.push(block_id))
            .or_insert(vec![block_id]);

        // have to upate the epoch and close on boundaries
        self.epochs
            .lock()
            .await
            .entry(block.proof.epoch)
            .and_modify(|epoch| {
                epoch
                    .slots
                    .entry(block.proof.timeslot)
                    .and_modify(|timeslot| {
                        timeslot.push(block_id);
                    })
                    .or_insert(vec![block_id]);
            });
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

    /// On block arrival add it to the recent block pool and check if it results in a confirmation
    // pub async fn apply_arrived_block(
    //     &mut self,
    //     block_id: &BlockId,
    //     _sender: &Sender<ProtocolMessage>,
    // ) -> Option<MetaBlock> {
    //     let block = match self.metablocks.get_mut(block_id) {
    //         Some(block) => block,
    //         None => return None,
    //     };
    //     block.state = BlockState::Arrived;
    //     let block = block.clone();

    //     // look for parent in recent blocks
    //     match self.metablocks.get_mut(&block.block.parent_id) {
    //         Some(first_parent_block) => {
    //             // solve on top of the block

    //             if self.node_type == NodeType::Farmer || self.node_type == NodeType::Gateway {
    //                 // sender
    //                 //     .send(ProtocolMessage::BlockChallenge {
    //                 //         parent_id: block.id,
    //                 //         challenge: block.block.tag,
    //                 //         base_time: block.block.timestamp,
    //                 //     })
    //                 //     .await;

    //                 // info!("Sent new block to solver");
    //             }

    //             // apply to pending ledger state and check for confimrations
    //             match first_parent_block.state {
    //                 BlockState::Arrived => {
    //                     // info!(
    //                     //     "Adding recent block {} with recent parent {}",
    //                     //     hex::encode(&block_id[0..8]),
    //                     //     hex::encode(&block.block.parent_id[0..8])
    //                     // );

    //                     // add a pointer in its parent
    //                     first_parent_block.children.push(*block_id);

    //                     let mut parent_block = first_parent_block.clone();
    //                     let mut branch_depth = 1;

    //                     // count recent blocks back
    //                     while let Some(next_parent_block) =
    //                         self.metablocks.get(&parent_block.block.parent_id)
    //                     {
    //                         // stop at the first confirmed block
    //                         if next_parent_block.state == BlockState::Confirmed {
    //                             break;
    //                         }

    //                         branch_depth += 1;
    //                         parent_block = next_parent_block.clone();
    //                     }

    //                     if branch_depth >= CONFIRMATION_DEPTH {
    //                         // apply the last parent and remove from recent blocks

    //                         // prune all other subtrees
    //                         // let siblings = self
    //                         //     .metablocks
    //                         //     .get_mut(&parent_block.block.parent_id)
    //                         //     .unwrap()
    //                         //     .children
    //                         //     .drain_filter(|child| child != &mut parent_block.id)
    //                         //     .collect();

    //                         // self.prune_branches(siblings);

    //                         // apply the new confirmed block
    //                         return self.confirm_block(&parent_block.id);
    //                     }

    //                     return Some(block);
    //                 }
    //                 BlockState::Confirmed => {
    //                     // special case that only occurs when the second block is applied to the genesis block
    //                     // as there are no uncofirmed blocks yet
    //                     // might also occur if a block arrives immediately after the parent is confirmed
    //                     // no need to check for confirmations since the branch is only length one

    //                     // info!(
    //                     //     "Adding recent block {} with confirmed parent {}",
    //                     //     hex::encode(&block_id[0..8]),
    //                     //     hex::encode(&block.block.parent_id[0..8])
    //                     // );

    //                     if block.block.parent_id == self.latest_block_hash {
    //                         first_parent_block.children.push(block.id);
    //                         return Some(block);
    //                     } else {
    //                         warn!("Recent block is late, its parent has already been confirmed!")
    //                     }
    //                 }
    //                 BlockState::New => {}
    //                 BlockState::Stray => {}
    //             }
    //         }
    //         None => {
    //             warn!(
    //                 "Do not have parent {} for recent block {}, dropping!",
    //                 hex::encode(&block_id[0..8]),
    //                 hex::encode(&block.block.parent_id[0..8])
    //             );
    //         }
    //     }

    //     // drop the block if it was not applied
    //     self.metablocks.remove(block_id);
    //     None

    //     // what about recent gossip for a node who is syncing?
    // }

    /// Apply a new block to the ledger by id (hash)
    // pub fn confirm_block(&mut self, block_id: &BlockId) -> Option<MetaBlock> {
    //     let block = self.metablocks.get_mut(block_id).unwrap();
    //     block.state = BlockState::Confirmed;
    //     self.latest_block_hash = *block_id;

    //     // update height
    //     self.height += 1;

    //     // update quality
    //     self.quality += block.block.get_quality() as u32;

    //     // update balances, get or add account
    //     self.balances
    //         .entry(*block_id)
    //         .and_modify(|balance| *balance += block.block.reward as usize)
    //         .or_insert(block.block.reward as usize);

    //     // Adds a pointer to this block id for the given index in the ledger
    //     self.confirmed_blocks_by_index
    //         .insert(self.height - 1, *block_id);

    //     info!("Confirmed block: {}", hex::encode(&block_id[0..8]));

    //     Some(block.clone())
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
