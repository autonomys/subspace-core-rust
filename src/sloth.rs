#![allow(dead_code)]

/*
A pure rust implementation of pysloth C internals
https://github.com/randomchain/pysloth/blob/master/sloth.c
by Mathias Michno

With extensions for a proof-of-replication
*/

use super::*;
use crate::Piece;
use rug::ops::NegAssign;
use rug::{integer::IsPrime, integer::Order, ops::BitXorFrom, Integer};
use std::ops::AddAssign;

/*  ToDo
 * only store expanded IV in integer form for encoding
 * revise sloth to mutate in place (Nazar)
 * remove unnessecary cloning (Nazar)
 * handle errors correctly if the data is larger than prime in sqrt_permutation (Nazar)
 * Ensure compiles for ARM -- gmp will be tricky (Nazar)
 * Ensure complies for Windows (Nazar)
 * use a different prime for each block for additional ASIC resistance
 * setup plotting tester script (with // plotting)
 * add in sloth art, progress bar, cli
 * implement for GPU in CUDA and OpenCL
 * implement parallel decoding to allow for smaller prime sizes and less encoding in //
 * ensure correct number of levels are applied for security guarantee
 *
 * test: data larger than prime should fail
 * test: hardcode in correct prime and ensure those are generated correctly (once prime is chosen)
*/

pub struct Sloth {
    pub block_size_bits: usize,
    pub block_size_bytes: usize,
    prime: Integer,
    exponent: Integer,
}

/// Finds the next smallest prime number
fn prev_prime(prime: &mut Integer) {
    if prime.is_even() {
        *prime -= 1
    } else {
        *prime -= 2
    }
    while prime.is_probably_prime(25) == IsPrime::No {
        *prime -= 2
    }
}

/// Returns (block, feedback) tuple given block index in a piece
fn piece_to_block_and_feedback(piece: &mut [Integer], index: usize) -> (&mut Integer, &Integer) {
    let (ends_with_feedback, starts_with_block) = piece.split_at_mut(index);
    let feedback = &ends_with_feedback[ends_with_feedback.len() - 1];
    (&mut starts_with_block[0], &feedback)
}

/// Returns (block, feedback) tuple given piece and optional feedback
fn piece_to_first_block_and_feedback<'a>(
    piece: &'a mut [Integer],
) -> (&'a mut Integer, &'a Integer) {
    let (first_block, remainder) = piece.split_at_mut(1);
    // At this point last block is already decoded, so we can use it as an IV to previous iteration
    let iv = &remainder[remainder.len() - 1];
    (&mut first_block[0], &iv)
}

impl Sloth {
    /// Inits sloth for a given prime size, determinsitcally deriving the largest prime and computing the exponent
    pub fn init(bits: usize) -> Self {
        let block_size_bits = bits;
        let block_size_bytes = bits / 8;

        let mut prime: Integer = Integer::from(Integer::u_pow_u(2, bits as u32)) - 1;
        prev_prime(&mut prime);
        while prime.mod_u(4) != 3 {
            prev_prime(&mut prime)
        }

        let mut exponent: Integer = prime.clone() + 1;
        exponent.div_exact_u_mut(4);

        Self {
            block_size_bits,
            block_size_bytes,
            prime,
            exponent,
        }
    }

    /// Computes the modular square root of data, for data smaller than prime (w.h.p.)
    pub fn sqrt_permutation(&self, data: &mut Integer) {
        // better error handling
        assert!(data.as_ref() < self.prime.as_ref());

        if data.jacobi(&self.prime) == 1 {
            data.pow_mod_mut(&self.exponent, &self.prime).unwrap();
            if data.is_odd() {
                data.neg_assign();
                data.add_assign(&self.prime);
            }
        } else {
            data.neg_assign();
            data.add_assign(&self.prime);
            data.pow_mod_mut(&self.exponent, &self.prime).unwrap();
            if data.is_even() {
                data.neg_assign();
                data.add_assign(&self.prime);
            }
        }
    }

    /// Inverts the sqrt permutation with a single squaring mod prime
    pub fn inverse_sqrt(&self, data: &mut Integer) {
        let is_odd = data.is_odd();
        data.square_mut();
        data.pow_mod_mut(&Integer::from(1), &self.prime).unwrap();
        if is_odd {
            data.neg_assign();
            data.add_assign(&self.prime);
        }
    }

    /// Sequentially encodes a 4096 byte piece s.t. a minimum amount of wall clock time elapses
    pub fn encode(&self, piece: &mut Piece, expanded_iv: ExpandedIV, layers: usize) -> Piece {
        let mut encoding: Piece = [0u8; PIECE_SIZE];
        let mut int_encoding: Vec<Integer> = vec![];

        // convert piece to integer representation
        piece.chunks_exact(self.block_size_bytes).for_each(|block| {
            int_encoding.push(Integer::from_digits(&block, Order::Lsf));
        });

        // init feedback as expanded IV
        let mut feedback = Integer::from_digits(&expanded_iv, Order::Lsf);

        // apply the block cipher
        for _ in 0..layers {
            for block in int_encoding.iter_mut() {
                // xor block with feedback
                block.bitxor_from(feedback);

                // apply sqrt permutation
                self.sqrt_permutation(block);

                // carry forward the feedback
                feedback = block.clone();
            }
        }

        // transform integer back to encoding
        int_encoding.iter().enumerate().for_each(|(i, block)| {
            block
                .to_digits::<u8>(Order::Lsf)
                .iter()
                .enumerate()
                .for_each(|(j, &byte)| encoding[i * self.block_size_bytes + j] = byte)
        });

        encoding
    }

    /// Sequentially decodes a 4096 byte encoding in time << encode time
    pub fn decode(&self, encoding: &mut Piece, expanded_iv: ExpandedIV, layers: usize) -> Piece {
        let mut piece: Piece = [0u8; PIECE_SIZE];
        let mut int_piece: Vec<Integer> = vec![];

        // convert encoding to integer representation
        encoding
            .chunks_exact(self.block_size_bytes)
            .for_each(|block| {
                int_piece.push(Integer::from_digits(&block, Order::Lsf));
            });

        for l in 0..layers {
            for i in (0..(PIECE_SIZE / self.block_size_bytes)).rev() {
                if i == 0 {
                    let (block, feedback) = piece_to_first_block_and_feedback(&mut int_piece);
                    self.inverse_sqrt(block);
                    if l != layers - 1 {
                        block.bitxor_from(feedback);
                    }
                } else {
                    let (block, feedback) = piece_to_block_and_feedback(&mut int_piece, i);
                    self.inverse_sqrt(block);
                    block.bitxor_from(feedback);
                }
            }
        }

        // remove the IV (last round)
        int_piece[0].bitxor_from(&Integer::from_digits(&expanded_iv, Order::Lsf));

        // transform integer back to piece
        int_piece.iter().enumerate().for_each(|(i, block)| {
            block
                .to_digits::<u8>(Order::Lsf)
                .iter()
                .enumerate()
                .for_each(|(j, &byte)| piece[i * self.block_size_bytes + j] = byte)
        });

        piece
    }
}

#[test]

fn test_random_data_for_all_primes() {
    use rug::{rand::RandState, Integer};
    use std::time::{SystemTime, UNIX_EPOCH};

    for &bits in [256, 512, 1024, 2048, 4096].iter() {
        let seed = Integer::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        );
        let mut rand = RandState::new();
        rand.seed(&seed);
        let data = Integer::from(Integer::random_bits(bits, &mut rand));
        let sloth = Sloth::init(bits as usize);
        let mut encoding = data.clone();
        sloth.sqrt_permutation(&mut encoding);
        let mut decoding = encoding.clone();
        sloth.inverse_sqrt(&mut decoding);

        println!("For prime and data of size {}", bits);
        println!("Prime: {}", sloth.prime.to_string_radix(10));
        println!("Data: {}", data.to_string_radix(10));
        println!("Encoding: {}", encoding.to_string_radix(10));
        println!("Decoding: {}\n\n", decoding.to_string_radix(10));

        assert_eq!(&data, &decoding);
    }
}

#[test]
fn test_random_piece_for_all_primes() {
    let iv = crypto::random_bytes_32();
    let expanded_iv = crypto::expand_iv(iv);

    for &bits in [256, 512, 1024, 2048, 4096].iter() {
        let mut piece = crypto::generate_random_piece();
        let sloth = Sloth::init(bits);
        let layers = PIECE_SIZE / sloth.block_size_bytes;
        let mut encoding = sloth.encode(&mut piece, expanded_iv, layers);
        let decoding = sloth.decode(&mut encoding, expanded_iv, layers);

        // println!("\nPiece is {:?}\n", piece.to_vec());
        // println!("\nDecoding is {:?}\n", decoding.to_vec());
        // println!("\nEncoding is {:?}\n", encoding.to_vec());

        assert_eq!(piece.to_vec(), decoding.to_vec());
    }
}
