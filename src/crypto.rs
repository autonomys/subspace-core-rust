#![allow(dead_code)]

use super::*;
use crate::{IV, Piece};
use ring::{digest};
// use rand::rngs::OsRng;
use rand::Rng;

// ToDo
    // fix workspaces
        // one lib crate w/lib.rs
        // one binary crate w/main.rs for app

pub fn random_bytes_32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes[..]);
    bytes
}

pub fn generate_random_piece() -> Piece {
    let mut bytes = [0u8; crate::PIECE_SIZE];
    rand::thread_rng().fill(&mut bytes[..]);
    bytes
}

/// Returns a deterministically generated genesis piece from a string seed.
pub fn genesis_piece_from_seed(seed: &str) -> Piece {
    let mut piece = [0u8; crate::PIECE_SIZE];
    let mut input = seed.as_bytes().to_vec();
    let mut block_offset = 0;
    for _ in 0..128 {
        input = digest_sha_256(&input).to_vec();
        piece[block_offset..(32 + block_offset)].clone_from_slice(&input[..32]);
        block_offset += 32;
    }
    piece
}

pub fn expand_iv(iv: IV) -> ExpandedIV {
    let mut expanded_iv: ExpandedIV = [0u8; PRIME_SIZE_BYTES];
    let mut feedback = iv.to_vec();
    let mut block_offset = 0;

    for _ in 0..PRIME_SIZE_BYTES/IV_SIZE {
        feedback = digest_sha_256(&feedback).to_vec();
        expanded_iv[block_offset..(IV_SIZE + block_offset)].clone_from_slice(&feedback[..32]);
        block_offset += IV_SIZE;
    }

    expanded_iv
}

/// Returns the SHA-256 hash of some input data as a fixed length array.
pub fn digest_sha_256(data: &[u8]) -> [u8; 32] {
    let mut array = [0u8; 32];
    let hash = digest::digest(&digest::SHA256, data).as_ref().to_vec();
    array.copy_from_slice(&hash[0..32]);
    array
}

pub fn create_hmac() {}

pub fn gen_keys() {}

pub fn create_merkle_tree() {}

pub fn get_merkle_proof() {}

pub fn validate_merkle_proof() {}
