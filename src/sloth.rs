/// A pure rust implementation of Sloth with extensions for a proof-of-replication
/// https://eprint.iacr.org/2015/366
/// based on pysloth C implementation by Mathias Michno
/// https://github.com/randomchain/pysloth/blob/master/sloth.c
use crate::{ExpandedIV, Piece, PIECE_SIZE};
use rayon::prelude::*;
use rug::ops::NegAssign;
use rug::{integer::IsPrime, integer::Order, ops::BitXorFrom, Integer};
use std::iter;
use std::ops::{AddAssign, Deref};

/*  ToDo
 * Ensure complies for Windows (Nazar)
 * use a different prime for each block for additional ASIC resistance
 * implement for GPU in CUDA with CGBN
 * implement for GPU in OpenCL with ff-cl-gen
 * ensure correct number of levels are applied for security guarantee
 * should this also take an IV?
 *
 * test: data larger than prime should fail
 * test: hardcode in correct prime and ensure those are generated correctly (once prime is chosen)
*/

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
fn piece_to_first_block_and_feedback(piece: &mut [Integer]) -> (&mut Integer, &Integer) {
    let (first_block, remainder) = piece.split_at_mut(1);
    // At this point last block is already decoded, so we can use it as an IV to previous iteration
    let iv = &remainder[remainder.len() - 1];
    (&mut first_block[0], &iv)
}

/// Converts a 4096 byte piece from an array of GMP big integers back to raw bytes
fn write_integers_to_array(integer_piece: &[Integer], piece: &mut [u8], block_size_bytes: usize) {
    integer_piece
        .iter()
        .flat_map(|integer| {
            let integer_bytes = integer.to_digits::<u8>(Order::Lsf);
            let integer_bytes_len = integer_bytes.len();
            integer_bytes
                .into_iter()
                .chain(iter::repeat(0).take(block_size_bytes - integer_bytes_len))
        })
        .zip(piece.iter_mut())
        .for_each(|(from_byte, to_byte)| {
            *to_byte = from_byte;
        });
}

#[derive(Debug)]
pub struct DataBiggerThanPrime;

enum Feedback<'a> {
    Iv(&'a Integer),
    Block(Integer),
}

impl<'a> Deref for Feedback<'a> {
    type Target = Integer;

    fn deref(&self) -> &Self::Target {
        match self {
            Feedback::Iv(integer) => integer,
            Feedback::Block(integer) => integer,
        }
    }
}

pub struct Sloth {
    pub block_size_bits: usize,
    pub block_size_bytes: usize,
    prime: Integer,
    exponent: Integer,
}

impl Sloth {
    /// Inits sloth for a given prime size, deterministically deriving the largest prime and computing the exponent
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
    pub fn sqrt_permutation(&self, data: &mut Integer) -> Result<(), DataBiggerThanPrime> {
        // better error handling
        if data.as_ref() >= self.prime.as_ref() {
            return Err(DataBiggerThanPrime);
        }

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

        Ok(())
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
    pub fn encode(
        &self,
        piece: &mut Piece,
        expanded_iv: &Integer,
        layers: usize,
    ) -> Result<(), DataBiggerThanPrime> {
        // convert piece to integer representation
        let mut integer_piece: Vec<Integer> = piece
            .chunks_exact(self.block_size_bytes)
            .map(|block| Integer::from_digits(&block, Order::Lsf))
            .collect();

        // init feedback as expanded IV
        let mut feedback = Feedback::Iv(expanded_iv);

        // apply the block cipher
        for _ in 0..layers {
            for block in integer_piece.iter_mut() {
                // xor block with feedback
                block.bitxor_from(feedback.deref());

                // apply sqrt permutation
                self.sqrt_permutation(block)?;

                // carry forward the feedback
                feedback = Feedback::Block(block.clone());
            }
        }

        // transform integers back to bytes
        write_integers_to_array(&integer_piece, piece, self.block_size_bytes);

        Ok(())
    }

    /// Sequentially decodes a 4096 byte encoding in time << encode time
    pub fn decode(&self, piece: &mut [u8], expanded_iv: ExpandedIV, layers: usize) {
        // convert encoding to integer representation
        let mut integer_piece: Vec<Integer> = piece
            .chunks_exact(self.block_size_bytes)
            .map(|block| Integer::from_digits(&block, Order::Lsf))
            .collect();

        for layer in 0..layers {
            for i in (1..(PIECE_SIZE / self.block_size_bytes)).rev() {
                let (block, feedback) = piece_to_block_and_feedback(&mut integer_piece, i);
                self.inverse_sqrt(block);
                block.bitxor_from(feedback);
            }
            let (block, feedback) = piece_to_first_block_and_feedback(&mut integer_piece);
            self.inverse_sqrt(block);
            if layer != layers - 1 {
                block.bitxor_from(feedback);
            }
        }

        // remove the IV (last round)
        integer_piece[0].bitxor_from(&Integer::from_digits(&expanded_iv, Order::Lsf));

        // transform integers back to bytes
        write_integers_to_array(&integer_piece, piece, self.block_size_bytes);
    }

    /// Decodes a 4096 byte encoding in parallel in time << encode time
    pub fn decode_parallel(&self, piece: &mut Piece, expanded_iv: ExpandedIV, layers: usize) {
        // convert encoding to integer representation
        let mut integer_piece: Vec<Integer> = piece
            .chunks_exact(self.block_size_bytes)
            .map(|block| Integer::from_digits(&block, Order::Lsf))
            .collect();

        for layer in 0..layers {
            let integer_piece_copy = integer_piece.clone();
            integer_piece
                .iter_mut()
                .skip(1)
                .rev()
                .zip(integer_piece_copy.iter().rev().skip(1))
                .par_bridge()
                .for_each(|(block, feedback)| {
                    self.inverse_sqrt(block);
                    block.bitxor_from(feedback);
                });

            let (block, feedback) = piece_to_first_block_and_feedback(&mut integer_piece);
            self.inverse_sqrt(block);
            if layer != layers - 1 {
                block.bitxor_from(feedback);
            }
        }

        // remove the IV (last round)
        integer_piece[0].bitxor_from(&Integer::from_digits(&expanded_iv, Order::Lsf));

        // transform integers back to bytes
        write_integers_to_array(&integer_piece, piece, self.block_size_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;

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
            sloth.sqrt_permutation(&mut encoding).unwrap();
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
        let integer_expanded_iv = Integer::from_digits(&expanded_iv, Order::Lsf);

        for &bits in [256, 512, 1024, 2048, 4096].iter() {
            let piece = crypto::generate_random_piece();
            let sloth = Sloth::init(bits);
            let layers = PIECE_SIZE / sloth.block_size_bytes;
            let mut encoding = piece.clone();
            sloth
                .encode(&mut encoding, &integer_expanded_iv, layers)
                .unwrap();
            let mut decoding = encoding.clone();
            sloth.decode(&mut decoding, expanded_iv, layers);

            // println!("\nPiece is {:?}\n", piece.to_vec());
            // println!("\nDecoding is {:?}\n", decoding.to_vec());
            // println!("\nEncoding is {:?}\n", encoding.to_vec());

            assert_eq!(piece.to_vec(), decoding.to_vec());

            let mut decoding = encoding.clone();
            sloth.decode_parallel(&mut decoding, expanded_iv, layers);

            assert_eq!(piece.to_vec(), decoding.to_vec());
        }
    }
}
