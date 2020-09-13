use crate::manager::ProtocolMessage;
use crate::plot::Plot;
use crate::{Piece, Tag, PIECE_COUNT};
use async_std::sync::{Receiver, Sender};
use log::*;
use std::fmt;
use std::fmt::Display;

pub enum FarmerMessage {
    /// Challenge to farmer for evaluation
    SlotChallenge {
        epoch: u64,
        timeslot: u64,
        epoch_randomness: [u8; 32],
        slot_challenge: [u8; 32],
        solution_range: u64,
    },
    StartFarming,
    StopFarming,
}

impl Display for FarmerMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::SlotChallenge { .. } => "SlotChallenge",
                Self::StartFarming => "StartFarming",
                Self::StopFarming => "StopFarming",
            }
        )
    }
}

#[derive(Copy, Clone)]
pub struct Solution {
    /// the epoch index for this block
    pub epoch: u64,
    /// the slot index for this block
    pub timeslot: u64,
    /// the randomness (from past epoch) for this block
    pub randomness: [u8; 32],
    /// derived piece_index
    pub piece_index: u64,
    /// index for audits and merkle proof
    pub proof_index: u64,
    /// tag for hmac(encoding||nonce) -> commitment
    pub tag: Tag,
    /// the full encoding
    pub encoding: Piece,
    /// Solution range for the eon block was generated at
    pub range: u64,
}

pub async fn run(
    timer_to_solver_rx: Receiver<FarmerMessage>,
    solver_to_main_tx: Sender<ProtocolMessage>,
    plot: &Plot,
) {
    let mut is_farming = true;

    info!("Solve loop is running...");
    while let Ok(message) = timer_to_solver_rx.recv().await {
        match message {
            FarmerMessage::SlotChallenge {
                epoch,
                timeslot,
                epoch_randomness,
                slot_challenge,
                solution_range,
            } => {
                if is_farming {
                    let tags = plot
                        .find_by_range(&slot_challenge, solution_range)
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
                            range: solution_range,
                        });
                    }
                    debug!(
                        "Found {} solutions for challenge {:?} and solution range ±{}",
                        solutions.len(),
                        hex::encode(&slot_challenge[0..8]),
                        solution_range / 2
                    );
                    solver_to_main_tx
                        .send(ProtocolMessage::BlockSolutions { solutions })
                        .await;
                }
            }
            FarmerMessage::StartFarming => is_farming = true,
            FarmerMessage::StopFarming => is_farming = false,
        }
    }
}
