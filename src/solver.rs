#![allow(dead_code)]

use super::*;
use crate::plot::Plot;
use async_std::sync::{Receiver, Sender};
use log::*;
use manager::ProtocolMessage;

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

    info!("Solve loop is running...");
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
                error!("Solve loop has received a protocol message other than BlockChallenge...");
            }
        }
    }
}
