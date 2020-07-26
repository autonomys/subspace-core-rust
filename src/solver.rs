#![allow(dead_code)]

use super::*;
use async_std::sync::{Receiver, Sender};
use async_std::task;
use log::*;
use manager::ProtocolMessage;
use std::time::Duration;

/* ToDo
 *
 * Change plot_size to block_height
 * Hanlde exceptions correctly
 *
*/

#[derive(Copy, Clone)]
pub struct Solution {
    pub challenge: [u8; 32], // hash of last block
    pub index: u64,          // derived piece_index
    pub tag: [u8; 32],       // tag for hmac(challenge||encoding)
    pub quality: u8,         // quality of the tag
    pub encoding: Piece,     // the full encoding
}

pub async fn run(
    main_to_sol_rx: Receiver<ProtocolMessage>,
    sol_to_main_tx: Sender<ProtocolMessage>,
    plot: &mut plot::Plot,
) {
    info!("Solve loop is running...");
    loop {
        match main_to_sol_rx.recv().await.unwrap() {
            ProtocolMessage::BlockChallenge { challenge } => {
                task::sleep(Duration::from_millis(SOLVE_WAIT_TIME_MS)).await;
                let solution = plot.solve(challenge, PLOT_SIZE).await;
                sol_to_main_tx
                    .send(ProtocolMessage::BlockSolution { solution })
                    .await;
            }
            _ => {
                error!("Solve loop has received a protocol message other than BlockChallenge...");
            }
        }
    }
}
