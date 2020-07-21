#![allow(dead_code)]

use super::*;
use async_std::sync::{Receiver, Sender};
use async_std::task;
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
    pub challenge: [u8; 32],
    pub index: u64,
    pub tag: [u8; 32],
    pub quality: u8,
    pub encoding: Piece,
}

pub async fn run(
    wait_time: u64,
    main_to_sol_rx: Receiver<ProtocolMessage>,
    sol_to_main_tx: Sender<ProtocolMessage>,
    plot: &mut plot::Plot,
) {
    println!("Solve loop is running...");
    loop {
        match main_to_sol_rx.recv().await.unwrap() {
            ProtocolMessage::BlockChallenge(challenge) => {
                task::sleep(Duration::from_millis(wait_time)).await;
                let solution = plot.solve(challenge, PLOT_SIZE).await;
                sol_to_main_tx
                    .send(ProtocolMessage::BlockSolution(solution))
                    .await;
            }
            _ => {
                panic!("Solve loop has received a protocol message other than BlockChallenge...");
            }
        }
    }
}
