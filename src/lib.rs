#![feature(try_blocks)]

pub mod console;
pub mod crypto;
pub mod ledger;
pub mod manager;
pub mod network;
pub mod plot;
pub mod plotter;
pub mod pseudo_wallet;
pub mod sloth;
pub mod solver;
pub mod utils;

pub const PRIME_SIZE_BITS: usize = 256;
pub const PRIME_SIZE_BYTES: usize = PRIME_SIZE_BITS / 8;
pub const IV_SIZE: usize = 32;
pub const PIECE_SIZE: usize = 4096;
pub const PIECE_COUNT: usize = 256;
pub const PLOT_SIZE: usize = 256 * 256;
pub const BLOCKS_PER_ENCODING: usize = PIECE_SIZE / PRIME_SIZE_BYTES;
pub const ENCODING_LAYERS_TEST: usize = 1;
pub const ENCODING_LAYERS_PROD: usize = BLOCKS_PER_ENCODING;
pub const PLOT_UPDATE_INTERVAL: usize = 10000;
pub const MAX_PEERS: usize = 8;
pub const TARGET_BLOCK_DELAY: f64 = 300.0;
pub const SOLVE_WAIT_TIME_MS: u64 = 1000;
pub const INITIAL_QUALITY_THRESHOLD: u8 = 0;
pub const DEGREE_OF_SIMULATION: usize = 1;
pub type Piece = [u8; PIECE_SIZE];
pub type IV = [u8; IV_SIZE];
pub type NodeID = IV;
pub type ExpandedIV = [u8; PRIME_SIZE_BYTES];
pub const DEV_GATEWAY_ADDR: &str = "127.0.0.1:8080";
pub const TEST_GATEWAY_ADDR: &str = "127.0.0.1:8080";
