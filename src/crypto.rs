#![allow(dead_code)]

use super::*;
use crate::{Piece, IV};
use ed25519_dalek::Keypair;
use merkle_tree_binary::Tree;
use ring::{digest, hmac};
use rand::rngs::OsRng;
use rand::Rng;

/* ToDo
 * 
 * Write tests
 * Make expanded IV simpler
 * Ensure hmac is used correctly
 * Ensure merkle tree is secure (two hash functions?)
 *
*/

/// Generates an array of 32 random bytes, for sims.
pub fn random_bytes_32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes[..]);
    bytes
}

/// Generates an array of 4096 randomm bytes, for generating simulated pieces.
pub fn generate_random_piece() -> Piece {
    let mut bytes = [0u8; crate::PIECE_SIZE];
    rand::thread_rng().fill(&mut bytes[..]);
    bytes
}

/// Returns the SHA-256 hash of some input data as a fixed length array.
pub fn digest_sha_256(data: &[u8]) -> [u8; 32] {
    let mut array = [0u8; 32];
    let hash = digest::digest(&digest::SHA256, data).as_ref().to_vec();
    array.copy_from_slice(&hash[0..32]);
    array
}

/// Returns the SHA-256 hash of some input data as a 32 byte vec.
pub fn digest_sha_256_simple(data: &[u8]) -> Vec<u8> {
    digest::digest(&digest::SHA256, data).as_ref().to_vec()
}

/// Returns the SHA-512 hash of some input data as a digest
pub fn digest_sha_512(data: &[u8]) -> digest::Digest {
    digest::digest(&digest::SHA512, data)
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

/// Expands a 32 byte node id into length of block size bytes
pub fn expand_iv(iv: IV) -> ExpandedIV {
    let mut expanded_iv: ExpandedIV = [0u8; PRIME_SIZE_BYTES];
    let mut feedback = iv.to_vec();
    let mut block_offset = 0;

    for _ in 0..PRIME_SIZE_BYTES / IV_SIZE {
        feedback = digest_sha_256(&feedback).to_vec();
        expanded_iv[block_offset..(IV_SIZE + block_offset)].clone_from_slice(&feedback[..32]);
        block_offset += IV_SIZE;
    }

    expanded_iv
}

/// Returns a hash bashed message authentication code unique to a message and challenge.
pub fn create_hmac(message: &[u8], challenge: &[u8]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, challenge);
    let mut array = [0u8; 32];
    let hmac = hmac::sign(&key, message).as_ref().to_vec();
    array.copy_from_slice(&hmac[0..32]);
    array
}

/// Returns a ED25519 key pair from a randomly generated seed.
pub fn gen_keys() -> ed25519_dalek::Keypair {
    let mut csprng = OsRng {};
    Keypair::generate(&mut csprng)
}

/// Deterministically builds a merkle tree with leaves the indices 0 to 255. Used to simulate the work done to prove and verity state blocks without having to build a state chain.
pub fn build_merkle_tree() -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut leaf_nodes: Vec<Vec<u8>> = Vec::new();
    for index in 0..256 {
        let bytes = (index as u8).to_le_bytes();
        let hash = digest_sha_256_simple(&bytes);
        leaf_nodes.push(hash);
    }
    let merkle_tree = Tree::new(&leaf_nodes, digest_sha_256_simple);
    let merkle_root = merkle_tree.get_root().to_vec();
    let mut merkle_proofs: Vec<Vec<u8>> = Vec::new();
    for index in 0..256 {
        let item = digest_sha_256(&(index as u8).to_le_bytes());
        let proof = merkle_tree.get_proof(&item).unwrap();
        merkle_proofs.push(proof);
    }

    (merkle_proofs, merkle_root)
}

/// Retrieves the merkle proof for a given challenge using the test merkle tree
pub fn get_merkle_proof(index: u64, merkle_proofs: &[Vec<u8>]) -> Vec<u8> {
    let merkle_index = (index % 256) as usize;
    merkle_proofs[merkle_index].clone()
}

/// Validates the merkle proof for a given challenge using the test merkle tree
pub fn validate_merkle_proof(index: usize, proof: &[u8], root: &[u8]) -> bool {
    let merkle_index = (index % 256) as u8;
    let target_item = digest_sha_256_simple(&merkle_index.to_le_bytes());
    Tree::check_proof(&root, &proof, &target_item, digest_sha_256_simple)
}
