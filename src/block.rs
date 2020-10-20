use crate::transaction::CoinbaseTx;
use crate::{
    crypto, sloth, state, BlockId, ContentId, ProofId, Tag, ENCODING_LAYERS_TEST,
    PIECES_PER_STATE_BLOCK, TX_BLOCKS_PER_PROPOSER_BLOCK,
};
use ed25519_dalek::{PublicKey, Signature};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use std::convert::TryInto;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Block {
    pub proof: Proof,
    pub coinbase_tx: CoinbaseTx,
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
        crypto::digest_sha_256(&self.to_bytes())
    }

    pub fn is_valid(
        &self,
        state: &state::State,
        epoch_randomness: &[u8; 32],
        slot_challenge: &[u8; 32],
        sloth: &sloth::Sloth,
    ) -> bool {
        // ensure we have the auxiliary data
        if self.data.is_none() {
            error!("Invalid block, missing auxiliary data!");
            return false;
        }

        // does the content reference the correct proof?
        if self.content.proof_id != self.proof.get_id() {
            error!("Invalid block, content and proof do not match!");
            return false;
        }

        let public_key = PublicKey::from_bytes(&self.proof.public_key).unwrap();
        let proof_signature = Signature::from_bytes(&self.content.proof_signature).unwrap();

        // is the proof signature valid?
        if public_key
            .verify_strict(&self.proof.get_id(), &proof_signature)
            .is_err()
        {
            error!("Invalid block, proof signature is invalid!");
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
            error!("Invalid block, content signature is invalid!");
            return false;
        }

        // is coinbase tx valid
        if !self.coinbase_tx.is_valid(&self.proof) {
            error!("Invalid block, coinbase tx is invalid!");
            return false;
        }

        // is coinbase first tx in the block
        if self.coinbase_tx.get_id() != self.content.refs[0] {
            error!("Invalid block, coinbase tx is not the first ref in content!");
            return false;
        }

        // is the epoch challenge correct?
        if epoch_randomness != &self.proof.randomness {
            error!("Invalid block, epoch randomness is incorrect!");
            return false;
        }

        // TODO: should verify that the solution range is correct for this timeslot

        // ensure block is sortitioned properly based on range
        let target = u64::from_be_bytes(slot_challenge[0..8].try_into().unwrap());
        let tag = u64::from_be_bytes(self.proof.tag);

        if self.content.parent_id.is_some() {
            // compute proposer block solution range
            let proposer_block_solution_range =
                self.proof.solution_range / (TX_BLOCKS_PER_PROPOSER_BLOCK + 1);
            let (lower, is_lower_overflowed) =
                target.overflowing_sub(proposer_block_solution_range / 2);
            let (upper, is_upper_overflowed) =
                target.overflowing_add(proposer_block_solution_range / 2);
            let within_proposer_block_solution_range = if is_lower_overflowed || is_upper_overflowed
            {
                upper <= tag || tag <= lower
            } else {
                lower <= tag && tag <= upper
            };

            if !within_proposer_block_solution_range {
                error!(
                    "Invalid proposer block, tag does not meet the proposer solution range ±{} for challenge {}!",
                    proposer_block_solution_range / 2,
                    hex::encode(&slot_challenge[0..8]),
                );
                return false;
            }
        } else {
            // does it fall into larger range (valid tx block)
            let (lower, is_lower_overflowed) =
                target.overflowing_sub(self.proof.solution_range / 2);
            let (upper, is_upper_overflowed) =
                target.overflowing_add(self.proof.solution_range / 2);
            let within_solution_range = if is_lower_overflowed || is_upper_overflowed {
                upper <= tag || tag <= lower
            } else {
                lower <= tag && tag <= upper
            };

            if !within_solution_range {
                error!(
                    "Invalid tx block, tag does not meet the tx block solution range ±{} for challenge {}!",
                    self.proof.solution_range / 2,
                    hex::encode(&slot_challenge[0..8]),
                );
                return false;
            }
        }

        // is the tag valid for the encoding and salt?
        let derived_tag = crypto::create_hmac(
            &self.data.as_ref().unwrap().encoding,
            &self.proof.nonce.to_le_bytes(),
        );
        let derived_tag: Tag = derived_tag[0..8].try_into().unwrap();
        if derived_tag != self.proof.tag {
            error!(
                "Invalid block, tag is invalid: {} vs {}",
                hex::encode(&self.proof.tag),
                hex::encode(&derived_tag)
            );
            // return false;
        }

        let state_block_index = self.proof.piece_index / PIECES_PER_STATE_BLOCK as u64;
        debug!(
            "Getting merkle proof for state block: {}.",
            state_block_index
        );
        let merkle_root = state
            .get_state_block_by_height(state_block_index)
            .unwrap()
            .piece_merkle_root;

        // is the merkle proof correct?
        if !crypto::validate_merkle_proof(
            self.data.as_ref().unwrap().piece_hash,
            &self.data.as_ref().unwrap().merkle_proof,
            &merkle_root,
        ) {
            error!("Invalid block, merkle proof is invalid!");
            return false;
        }

        // is the encoding valid for the public key and index?
        let id = crypto::digest_sha_256(&self.proof.public_key);
        let expanded_iv = crypto::expand_iv(id);
        let layers = ENCODING_LAYERS_TEST;
        let mut decoding = self.data.as_ref().unwrap().encoding.clone();

        sloth.decode(decoding.as_mut(), expanded_iv, layers);
        let decoding_hash = crypto::digest_sha_256(&decoding);
        if &self.data.as_ref().unwrap().piece_hash != &decoding_hash {
            error!("Invalid block, encoding is invalid");
            // utils::compare_bytes(&proof.encoding, &proof.encoding, &decoding);
            return false;
        }

        true
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Proof {
    // TODO: should include parent proof id
    /// epoch challenge
    pub randomness: ProofId,
    /// epoch index
    pub epoch: u64,
    /// time slot
    pub timeslot: u64,
    /// farmers public key
    pub public_key: [u8; 32],
    /// hmac of encoding with a nonce
    pub tag: Tag,
    /// nonce for salting the tag
    pub nonce: u64,
    /// index of piece for encoding
    pub piece_index: u64,
    // TODO: This property needs to be verified somehow when we receive a proof
    /// Solution range for the eon block was generated at
    pub solution_range: u64,
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
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Content {
    /// id of matching proof
    pub proof_id: ProofId,
    /// content id of parent block that is seen as head of the longest chain (only for proposer block)
    pub parent_id: Option<ContentId>,
    /// signature of the proof with same public key
    pub proof_signature: Vec<u8>,
    /// when this block was created (from Nodes local view)
    pub timestamp: u64,
    /// ids of all unseen tx blocks (proposer blocks) or all unseen txs (tx block)
    /// first ref is always the coinbase tx for this block
    pub refs: Vec<[u8; 32]>,
    // TODO: account for farmers who sign the same proof with two different contents
    /// signature of the content with same public key
    pub signature: Vec<u8>,
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
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Data {
    /// the encoding of the piece with public key
    pub encoding: Vec<u8>,
    /// merkle proof showing piece is in the ledger
    pub merkle_proof: Vec<u8>,
    /// hash of the original piece used for the encoding
    pub piece_hash: [u8; 32],
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
