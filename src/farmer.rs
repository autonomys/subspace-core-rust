use crate::manager::ProtocolMessage;
use crate::plot::Plot;
use crate::{Piece, Tag};
use async_std::sync::{Receiver, Sender};
use log::*;
use std::convert::TryInto;
use std::fmt;
use std::fmt::Display;

pub enum FarmerMessage {
    /// Challenge to farmer for evaluation
    SlotChallenge {
        epoch_index: u64,
        timeslot: u64,
        randomness: [u8; 32],
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

#[derive(Clone)]
pub struct Solution {
    /// the epoch index for this block
    pub epoch_index: u64,
    /// the time slot for this block
    pub timeslot: u64,
    /// the randomness (from past epoch) for this block
    pub randomness: [u8; 32],
    /// index of the piece as it appears in the state
    pub piece_index: u64,
    /// tag for hmac(encoding||nonce) -> commitment
    pub tag: Tag,
    /// the full encoding
    pub encoding: Piece,
    /// merkle proof that encoded piece is in the state chain
    pub merkle_proof: Vec<u8>,
    /// Solution range for the eon block was generated at
    pub solution_range: u64,
    /// target for this challenge
    pub target: [u8; 8],
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
                epoch_index,
                timeslot,
                randomness,
                slot_challenge,
                solution_range,
            } => {
                if is_farming {
                    let target = slot_challenge[0..8].try_into().unwrap();
                    let tags = plot.find_by_range(target, solution_range).await.unwrap();
                    let mut solutions: Vec<Solution> = Vec::with_capacity(tags.len());
                    for (tag, piece_index) in tags.into_iter() {
                        let (encoding, merkle_proof) = plot.read(piece_index).await.unwrap();
                        solutions.push(Solution {
                            epoch_index,
                            timeslot,
                            randomness,
                            piece_index: piece_index as u64,
                            tag,
                            encoding,
                            merkle_proof,
                            solution_range,
                            target,
                        });
                    }

                    debug!(
                        "Found {} solutions for challenge {:?} and solution range ±{} at timeslot {}",
                        solutions.len(),
                        hex::encode(&slot_challenge[0..8]),
                        solution_range / 2,
                        timeslot,
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
