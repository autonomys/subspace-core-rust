use crate::state::MerkleRoot;
use crate::{ExpandedIV, Piece, IV, IV_SIZE, PRIME_SIZE_BYTES};
use ed25519_dalek::Keypair;
use merkle_tree_binary::Tree;
use rand::rngs::{OsRng, StdRng};
use rand::Rng;
use rand_core::SeedableRng;
use ring::{digest, hmac};
use std::convert::TryInto;

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
pub fn gen_keys_random() -> ed25519_dalek::Keypair {
    let mut csprng = OsRng {};
    Keypair::generate(&mut csprng)
}

/// Returns an ED25519 key pair from a user provided seed
pub fn gen_keys_from_seed(seed: u64) -> ed25519_dalek::Keypair {
    let mut rng = StdRng::seed_from_u64(seed);
    Keypair::generate(&mut rng)
}

pub fn create_merkle_tree(ids: &[[u8; 32]]) -> (MerkleRoot, Vec<Vec<u8>>) {
    let vec_ids: Vec<Vec<u8>> = ids.iter().map(|id| id.to_vec()).collect();
    let merkle_tree = Tree::new(&vec_ids, digest_sha_256_simple);

    let merkle_proofs: Vec<Vec<u8>> = vec_ids
        .iter()
        .map(|id| merkle_tree.get_proof(&id).unwrap())
        .collect();

    let merkle_root = merkle_tree
        .get_root()
        .try_into()
        .expect("Hash is always 32 bytes");

    (merkle_root, merkle_proofs)
}

/// Validates the merkle proof for a given challenge using the test merkle tree
pub fn validate_merkle_proof(
    piece_hash: [u8; 32],
    piece_proof: &[u8],
    piece_merkle_root: &[u8],
) -> bool {
    Tree::check_proof(
        &piece_merkle_root,
        &piece_proof,
        &piece_hash,
        digest_sha_256_simple,
    )
}

/// Creates a state update that will add exactly one new piece with length encoding for deriving genesis state
pub fn genesis_data_from_seed(seed: [u8; 32]) -> Vec<u8> {
    let mut piece = [0u8; crate::PIECE_SIZE];
    let mut input = seed.to_vec();
    let mut block_offset = 0;
    for _ in 0..128 {
        input = digest_sha_256(&input).to_vec();
        piece[block_offset..(32 + block_offset)].clone_from_slice(&input[..32]);
        block_offset += 32;
    }
    piece[0..4094].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_genesis_data_from_seed() {
        let seed = random_bytes_32();
        let genesis_data = genesis_data_from_seed(seed);

        assert_eq!(genesis_data.len(), 4094)
    }
}
