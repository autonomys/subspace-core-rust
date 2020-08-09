#![allow(dead_code)]

use super::*;
use crate::plot::Plot;
use async_std::sync::{Receiver, Sender};
use log::*;
use manager::ProtocolMessage;
use rug::Float;
use std::convert::TryInto;
use std::io::Write;

/* ToDo
 *
 * Change plot_size to block_height
 * Hanlde exceptions correctly
 *
 * Rational farmers will wait until the deadline has expired (or the block is full) before sharing
 * Later we can add in a node who predicts into the future
 *
*/

#[derive(Copy, Clone)]
pub struct Solution {
    pub parent: [u8; 32],    // hash of last block
    pub challenge: [u8; 32], // tag of last block
    pub base_time: u128,     // timestamp of last block
    pub piece_index: u64,    // derived piece_index
    pub proof_index: u64,    // index for audits and merkle proof
    pub tag: [u8; 32],       // tag for hmac(challenge||encoding)
    pub delay: u32,          // quality of the tag
    pub encoding: Piece,     // the full encoding
}

pub async fn run(
    main_to_sol_rx: Receiver<ProtocolMessage>,
    sol_to_main_tx: Sender<ProtocolMessage>,
    plot: &Plot,
) {
    let bits: u32 = 32;
    let target_value = 2u32.pow(bits - (REPLICATION_FACTOR as f64).log2() as u32);

    if !SOLVE_V2 {
        info!("Solve v1 loop is running...");
        loop {
            match main_to_sol_rx.recv().await.unwrap() {
                ProtocolMessage::BlockChallenge {
                    parent_id,
                    challenge,
                    base_time,
                } => {
                    // choose the correct "virtual" piece
                    let base_index = utils::modulo(&challenge, PIECE_COUNT);
                    let mut solutions: Vec<Solution> = Vec::new();
                    // read each "virtual" encoding of that piece
                    for i in 0..REPLICATION_FACTOR {
                        let index = base_index + (i * REPLICATION_FACTOR) as usize;
                        let encoding = plot.read(index).await.unwrap();
                        let tag = crypto::create_hmac(&encoding[..], &challenge);
                        let sample = utils::bytes_le_to_u32(&tag[0..4]);
                        let distance = (sample as f64).log2() - (target_value as f64).log2();
                        let delay = (TARGET_BLOCK_DELAY * 2f64.powf(distance)) as u32;

                        solutions.push(Solution {
                            parent: parent_id,
                            challenge,
                            base_time,
                            piece_index: index as u64,
                            proof_index: base_index as u64,
                            tag,
                            delay,
                            encoding,
                        })
                    }

                    // sort the solutions so that smallest delay is first
                    solutions.sort_by_key(|s| s.delay);
                    let solutions = solutions[0..DEGREE_OF_SIMULATION].to_vec();

                    // info!("Solver is sending solutions to main");

                    sol_to_main_tx
                        .send(ProtocolMessage::BlockSolutions { solutions })
                        .await;
                }
                _ => {
                    error!(
                        "Solve loop has received a protocol message other than BlockChallenge..."
                    );
                }
            }
        }
    } else {
        loop {
            match main_to_sol_rx.recv().await.unwrap() {
                ProtocolMessage::BlockChallenge {
                    parent_id,
                    challenge,
                    base_time,
                } => {
                    let challenge_hash = crypto::digest_sha_256_simple(&challenge);
                    let challenge_u64 =
                        u64::from_le_bytes(challenge_hash[0..8].try_into().unwrap());
                    let mut solutions: Vec<Solution> = Vec::new();
                    let (best_tag, index) = plot.find_by_tag(challenge_u64).await.unwrap();
                    // choose the correct "virtual" piece
                    let base_index = index % PIECE_COUNT;
                    let encoding = plot.read(index).await.unwrap();
                    let mut tag = [0u8; 32];
                    tag.as_mut().write_all(&best_tag.to_le_bytes()).unwrap();

                    // For desired delay of 300 ms
                    let mean_delay: f64 = 300.0;

                    let max: f64 = 64.0;
                    let base = ((PIECE_COUNT * REPLICATION_FACTOR) as f64).log2(); // -> 16 here
                    let delta = max - base; // 64 - 16 = 48
                    let f_target = Float::with_val(10, delta); // load as big float with 10 decimals of precisions

                    debug!("challenge_u64 {} best_tag {}", challenge_u64, best_tag);
                    // XOR the challenge and the tag
                    // the more similar they are (closer), the smaller the result, the shorter the delay
                    let quality = challenge_u64 ^ best_tag;
                    let f_quality = Float::with_val(10, quality.to_be()); // load as a big float with same precision

                    debug!(
                        "f_quality.log2() {} f_target {}",
                        f_quality.clone().log2(),
                        f_target
                    );
                    // quality is still a very big number so we take log2 to bring it down to same domain as the target
                    // then we subtract the two to get the distance (in log2 domain)
                    let f_distance = f_quality.log2() - f_target;

                    // delay shdoul mean x 2^distance
                    let delay = mean_delay * f_distance.exp2();
                    let delay = delay.to_integer().unwrap().to_u32().unwrap();

                    debug!("delay {}", delay);

                    solutions.push(Solution {
                        parent: parent_id,
                        challenge,
                        base_time,
                        piece_index: index as u64,
                        proof_index: base_index as u64,
                        tag,
                        delay,
                        encoding,
                    });

                    // sort the solutions so that smallest delay is first
                    solutions.sort_by_key(|s| s.delay);

                    // info!("Solver is sending solutions to main");

                    sol_to_main_tx
                        .send(ProtocolMessage::BlockSolutions { solutions })
                        .await;
                }
                _ => {
                    error!(
                        "Solve loop has received a protocol message other than BlockChallenge..."
                    );
                }
            }
        }
    }
}
