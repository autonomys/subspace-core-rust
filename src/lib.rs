#![feature(try_blocks)]
#![feature(const_int_pow)]
#![feature(drain_filter)]
#![feature(map_first_last)]
#![feature(map_into_keys_values)]

use async_std::sync::{Arc, Mutex};
use static_assertions::const_assert;
use std::collections::HashMap;
use std::time::Duration;

pub mod block;
pub mod console;
pub mod crypto;
pub mod farmer;
pub mod ipc;
pub mod ledger;
pub mod manager;
pub mod metablocks;
pub mod network;
pub mod plot;
pub mod plotter;
pub mod pseudo_wallet;
pub mod rpc;
pub mod sloth;
pub mod state;
pub mod timer;
pub mod transaction;
pub mod utils;

// TODO: Should make into actual structs
pub type Piece = [u8; PIECE_SIZE];
pub type PieceId = [u8; 32];
pub type PieceIndex = u64;
pub type IV = [u8; IV_SIZE];
pub type ExpandedIV = [u8; PRIME_SIZE_BYTES];
pub type PublicKey = [u8; 32];
pub type NodeID = [u8; 32];
pub type Tag = [u8; 8];
pub type BlockId = [u8; 32];
pub type ProofId = [u8; 32];
pub type ContentId = [u8; 32];
pub type EpochRandomness = Arc<Mutex<HashMap<u64, [u8; 32]>>>;
pub type EpochChallenge = [u8; 32];
pub type SlotChallenge = [u8; 32];

pub const PRIME_SIZE_BITS: usize = 256;
pub const PRIME_SIZE_BYTES: usize = PRIME_SIZE_BITS / 8;
pub const IV_SIZE: usize = 32;
pub const PIECE_SIZE: usize = 4096;
pub const PIECE_COUNT: usize = 256;
pub const BLOCKS_PER_ENCODING: usize = PIECE_SIZE / PRIME_SIZE_BYTES;
pub const ENCODING_LAYERS_TEST: usize = 1;
pub const ENCODING_LAYERS_PROD: usize = BLOCKS_PER_ENCODING;
pub const PLOT_UPDATE_INTERVAL: usize = 10000;
pub const MIN_PEERS: usize = 1;
pub const MAX_PEERS: usize = 20;
pub const MIN_CONTACTS: usize = 0;
pub const MAX_CONTACTS: usize = 100;
pub const BLOCK_LIST_SIZE: usize = 100;
// TODO: Is this a good value?
pub const MAINTAIN_PEERS_INTERVAL: Duration = Duration::from_secs(60);
pub const CONFIRMATION_DEPTH: usize = 6;
pub const DEV_GATEWAY_ADDR: &str = "127.0.0.1:8081";
pub const TEST_GATEWAY_ADDR: &str = "127.0.0.1:8080";
pub const DEV_WS_ADDR: &str = "127.0.0.1:8880";
pub const CONSOLE: bool = false;
pub const BLOCK_REWARD: u64 = 1;
pub const MAX_EARLY_TIMESLOTS: u64 = 10;
pub const MAX_LATE_TIMESLOTS: u64 = 10;
pub const TIMESLOT_DURATION: u64 = 1000;
pub const CHALLENGE_LOOKBACK_EPOCHS: u64 = 1;
/// Time in epochs
pub const EPOCH_CLOSE_WAIT_TIME: u64 = 1;
pub const TIMESLOTS_PER_EPOCH: u64 = 32;

/// Solution Range
pub const TX_BLOCKS_PER_PROPOSER_BLOCK: u64 = 4;
pub const TIMESLOTS_PER_BLOCK: u64 = 1;
pub const TIMESLOTS_PER_PROPOSER_BLOCK: u64 = 5;
pub const PROPOSER_BLOCKS_PER_EON: u64 = 2016 * 4;
pub const ADJUSTMENT_FACTOR: f64 = 1.105;
pub const EXPECTED_TIMESLOTS_PER_EON: u64 = (TIMESLOTS_PER_PROPOSER_BLOCK as f64
    * PROPOSER_BLOCKS_PER_EON as f64
    * ADJUSTMENT_FACTOR) as u64;
// TODO: compute dynamically from plot size for testing
pub const INITIAL_SOLUTION_RANGE: u64 = u64::MAX / PLOT_SIZE as u64 / TIMESLOTS_PER_BLOCK;
pub const SOLUTION_RANGE_UPDATE_DELAY_IN_TIMESLOTS: u64 = 10;

/// State Encoding
/// 8k
pub const STATE_BLOCK_SIZE_IN_BYTES: usize = 8 * 1024;
/// 2
pub const PIECES_PER_STATE_BLOCK: usize = STATE_BLOCK_SIZE_IN_BYTES / PIECE_SIZE;
/// 128
pub const GENESIS_STATE_BLOCKS: usize = (1024 * 1024) / STATE_BLOCK_SIZE_IN_BYTES;
/// 256
pub const GENESIS_PIECE_COUNT: usize = PIECES_PER_STATE_BLOCK * GENESIS_STATE_BLOCKS;

pub const PLOT_SIZE: usize = GENESIS_PIECE_COUNT;
pub const IPC_SOCKET_FILE: &str = "ipc.socket";

// Assertions about acceptable values for above parameters:
// Lookback should always be at least one
const_assert!(EPOCH_CLOSE_WAIT_TIME >= 1);
// Epoch must be closed by the time we do lookback to it
const_assert!(CHALLENGE_LOOKBACK_EPOCHS >= EPOCH_CLOSE_WAIT_TIME);

pub const EPOCH_GRACE_PERIOD: Duration =
    Duration::from_millis(TIMESLOTS_PER_EPOCH * TIMESLOT_DURATION);
