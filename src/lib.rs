pub mod crypto;
pub mod plot;
pub mod plotter;
pub mod sloth;
pub mod utils;

pub const PRIME_SIZE_BITS: usize = 256;
pub const PRIME_SIZE_BYTES: usize = PRIME_SIZE_BITS / 8;
pub const IV_SIZE: usize = 32;
pub const PIECE_SIZE: usize = 4096;
pub const UPDATE_INTERVAL: usize = 10000;
pub type Piece = [u8; PIECE_SIZE];
pub type IV = [u8; IV_SIZE];
pub type ExpandedIV = [u8; PRIME_SIZE_BYTES];
