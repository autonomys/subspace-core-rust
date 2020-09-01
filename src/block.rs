#![allow(dead_code)]

use super::*;
use ed25519_dalek::PublicKey;
use ed25519_dalek::Signature;
use log::*;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::convert::TryInto;

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
        _epoch_randomness: &[u8; 32],
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

        // // is the epoch challenge correct?
        // if epoch_randomness != &self.proof.randomness {
        //     warn!("Invalid block, epoch randomness is incorrect!");
        //     return false;
        // }

        // is the tag within range of the slot challenge?
        // let slot_seed = [
        //     &epoch_randomness[..],
        //     &(self.proof.timeslot % TIMESLOTS_PER_EPOCH).to_le_bytes()[..],
        // ]
        // .concat();
        // let slot_challenge = crypto::digest_sha_256_simple(&slot_seed);

        let target = u64::from_be_bytes(slot_challenge[0..8].try_into().unwrap());
        let (_distance, _) = target.overflowing_sub(self.proof.tag);

        // if distance > SOLUTION_RANGE {
        //     warn!("Invalid block, solution does not meet the difficulty target!");
        //     return false;
        // }

        // // is the tag valid for the encoding and salt?
        // let tag_hash = crypto::create_hmac(
        //     &self.data.as_ref().unwrap().encoding,
        //     &self.proof.nonce.to_le_bytes(),
        // );
        // let derived_tag = u64::from_be_bytes(tag_hash[0..8].try_into().unwrap());
        // if derived_tag.cmp(&self.proof.tag) != Ordering::Equal {
        //     warn!("Invalid block, tag is invalid");
        //     return false;
        // }

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
    pub nonce: u128,
    /// index of piece for encoding
    pub piece_index: u64,
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
    /// ids of all parent blocks not yet seen
    pub parent_ids: Vec<ContentId>,
    /// id of matching proof
    pub proof_id: ProofId,
    /// signature of the proof with same public key
    pub proof_signature: Vec<u8>,
    /// when this block was created (from Nodes local view)
    pub timestamp: u64,
    // TODO: Should be a vec of TX IDs
    /// ids of all unseen transactions seen by this block
    pub tx_ids: Vec<u8>,
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

    pub fn is_valid() {
        // is proof signature valid (requires public key)

        // is timestamp valid

        // is signature valid (requires public key)
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Data {
    /// the encoding of the piece with public key
    pub encoding: Vec<u8>,
    /// merkle proof showing piece is in the ledger
    pub merkle_proof: Vec<u8>,
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