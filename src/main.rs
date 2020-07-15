
mod crypto;
mod sloth;
mod utils;

pub const PRIME_SIZE_BITS: usize = 256;
pub const PRIME_SIZE_BYTES: usize = PRIME_SIZE_BITS / 8;
pub const IV_SIZE: usize = 32;
pub const PIECE_SIZE: usize = 4096;
pub type Piece = [u8; PIECE_SIZE];
pub type Encoding = [u8; PIECE_SIZE];
pub type IV = [u8; IV_SIZE];
pub type ExpandedIV = [u8; PRIME_SIZE_BYTES];

// ToDo
    // plotter benchmarking

fn main() {
    println!("Hello, world!");
}
