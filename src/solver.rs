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
    pub challenge: [u8; 32], // hash of last block
    pub base_time: u128,     // timestamp of last block
    pub index: u64,          // derived piece_index
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
    let replication_factor = (PLOT_SIZE / PIECE_COUNT) as u32;
    let target_value = 2u32.pow(bits - (replication_factor as f64).log2() as u32);

    info!("Solve loop is running...");
    loop {
        match main_to_sol_rx.recv().await.unwrap() {
            ProtocolMessage::BlockChallenge {
                challenge,
                base_time,
            } => {
                let solutions = plot
                    .solve(
                        challenge,
                        base_time,
                        PIECE_COUNT,
                        replication_factor,
                        target_value,
                    )
                    .await;

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
