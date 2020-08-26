#![allow(dead_code)]

use super::*;
use crate::plot::Plot;
use async_std::sync::{Receiver, Sender};
use log::*;
use manager::ProtocolMessage;

#[derive(Copy, Clone)]
pub struct Solution {
    pub epoch: u64,           // the epoch index for this block
    pub timeslot: u64,        // the slot index for this block
    pub randomness: [u8; 32], // the randomness (from past epoch) for this block
    pub piece_index: u64,     // derived piece_index
    pub proof_index: u64,     // index for audits and merkle proof
    pub tag: u64,             // tag for hmac(encoding||nonce) -> commitment
    pub encoding: Piece,      // the full encoding
}

pub async fn run(
    timer_to_solver_rx: Receiver<ProtocolMessage>,
    solver_to_main_tx: Sender<ProtocolMessage>,
    plot: &Plot,
) {
    info!("Solve loop is running...");
    loop {
        match timer_to_solver_rx.recv().await.unwrap() {
            ProtocolMessage::SlotChallenge {
                epoch,
                timeslot,
                epoch_randomness,
                slot_challenge,
            } => {
                // TODO: make range dynamic based on difficulty resets
                let tags: Vec<(u64, usize)> = plot
                    .find_by_range(&slot_challenge, SOLUTION_RANGE)
                    .await
                    .unwrap();
                let mut solutions: Vec<Solution> = Vec::with_capacity(tags.len());
                for (tag, piece_index) in tags.into_iter() {
                    let proof_index = piece_index % PIECE_COUNT;
                    let encoding = plot.read(piece_index).await.unwrap();

                    solutions.push(Solution {
                        epoch,
                        timeslot,
                        randomness: epoch_randomness,
                        piece_index: piece_index as u64,
                        proof_index: proof_index as u64,
                        tag,
                        encoding,
                    });
                }

                info!("Found {} solutions for challenge", solutions.len());

                solver_to_main_tx
                    .send(ProtocolMessage::BlockSolutions { solutions })
                    .await;
            }
            _ => {}
        }
    }
}
