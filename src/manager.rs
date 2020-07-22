#![allow(dead_code)]

use super::*;
use async_std::sync::{Receiver, Sender};
use futures::join;
use ledger::{Block, BlockStatus, Proof};
use solver::Solution;

pub enum ProtocolMessage {
    BlockChallenge([u8; 32]), // Main sends challenge to solver for evaluation
    BlockSolution(Solution),  // Solver sends solution back to main for application
}

pub async fn run(
    genesis_piece_hash: [u8; 32],
    quality_threshold: u8,
    binary_public_key: [u8; 32],
    keys: ed25519_dalek::Keypair,
    merkle_proofs: Vec<Vec<u8>>,
    tx_payload: Vec<u8>,
    ledger: &mut ledger::Ledger,
    any_to_main_rx: Receiver<ProtocolMessage>,
    main_to_sol_tx: Sender<ProtocolMessage>,
) {
    let protocol_listener = async {
        println!("Main protocol loop is running...");
        loop {
            if let Some(message) = any_to_main_rx.recv().await.ok() {
                match message {
                    ProtocolMessage::BlockSolution(solution) => {
                        println!(
                            "Received a solution for challenge: {}",
                            hex::encode(&solution.challenge)
                        );
                        // check solution quality is high enough
                        if solution.quality < quality_threshold {
                            println!("Solution to block challenge does not meet quality threshold, ignoring");
                            continue;
                        }

                        // sign tag and create block
                        let block = Block::new(
                            solution.challenge,
                            solution.tag,
                            binary_public_key,
                            keys.sign(&solution.tag).to_bytes().to_vec(),
                            tx_payload.clone(),
                        );

                        // add block to ledger
                        match ledger.apply_block_by_id(&block) {
                            BlockStatus::Applied => {
                                // valid extension to the ledger, gossip to the network
                                println!("Applied new block generated locally to the ledger!");
                                // block.print();

                                let _proof = Proof::new(
                                    solution.encoding,
                                    crypto::get_merkle_proof(solution.index, &merkle_proofs),
                                    solution.index,
                                );

                                let solve = main_to_sol_tx
                                    .send(ProtocolMessage::BlockChallenge(block.get_id()));
                                // let gossip = main_to_net_tx.send(ProtocolMessage::BlockProposal(
                                //     FullBlock { block, proof },
                                // ));

                                join!(solve
                                    // , gossip
                                );
                            }
                            BlockStatus::Invalid => {
                                // illegal extension to the ledger, ignore
                                // may have applied a better block received over the network while the solution was being generated
                                println!("Attempted to add locally generated block to the ledger, but was no longer valid");
                            }
                            BlockStatus::Pending => {
                                // this should not happen, control flow logic error
                                panic!("A block generated locally does not have a known parent...")
                            }
                        }
                    }
                    _ => panic!("Main protocol listener has received an unknown protocol message!"),
                }
            }
        }
    };

    let protocol_startup = async {
        println!("Calling protocol startup");

        main_to_sol_tx
            .send(ProtocolMessage::BlockChallenge(genesis_piece_hash))
            .await;
    };

    join!(protocol_listener, protocol_startup);
}
