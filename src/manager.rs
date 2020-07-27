#![allow(dead_code)]

use super::*;
use async_std::sync::{Receiver, Sender};
use async_std::task;
use console::AppState;
use futures::join;
use ledger::{Block, BlockStatus, FullBlock, Proof};
use log::*;
use network::NodeType;
use solver::Solution;
use std::net::SocketAddr;
use std::time::Duration;

// TODO: Split this into multiple enums
pub enum ProtocolMessage {
    /// On sync, main forwards block request to Net for tx from self to peer
    BlockRequest {
        index: u32,
    },
    /// peer receives at Net and forwards request to main for fetch
    BlockRequestFrom {
        node_addr: SocketAddr,
        index: u32,
    },
    /// peer main forwards response back to Net for tx
    BlockResponseTo {
        node_addr: SocketAddr,
        block: Option<Block>,
        index: u32,
    },
    /// self receives at Net and forwards response back to Main
    BlockResponse {
        block: Block,
    },
    /// Net receives new full block, validates/applies, sends back to net for re-gossip
    BlockProposalRemote {
        full_block: FullBlock,
        peer_addr: SocketAddr,
    },
    /// A valid full block has been produced locally and needs to be gossiped
    BlockProposalLocal {
        full_block: FullBlock,
    },
    /// Main sends challenge to solver for evaluation
    BlockChallenge {
        challenge: [u8; 32],
    },
    /// Solver sends solution back to main for application
    BlockSolution {
        solution: Solution,
    },
    StateUpdateRequest,
    StateUpdateResponse {
        state: AppState,
    },
}

pub async fn run(
    node_type: NodeType,
    genesis_piece_hash: [u8; 32],
    binary_public_key: [u8; 32],
    keys: ed25519_dalek::Keypair,
    merkle_proofs: Vec<Vec<u8>>,
    tx_payload: Vec<u8>,
    ledger: &mut ledger::Ledger,
    any_to_main_rx: Receiver<ProtocolMessage>,
    main_to_net_tx: Sender<ProtocolMessage>,
    main_to_sol_tx: Sender<ProtocolMessage>,
    state_sender: crossbeam_channel::Sender<AppState>,
) {
    // let node_type_string = node_type as String;

    // convert node_type to string
    // convert node_id to string
    //

    let protocol_listener = async {
        info!("Main protocol loop is running...");
        loop {
            if let Some(message) = any_to_main_rx.recv().await.ok() {
                match message {
                    ProtocolMessage::BlockRequestFrom { node_addr, index } => {
                        let block = ledger.get_block_by_index(index);
                        let message = ProtocolMessage::BlockResponseTo {
                            node_addr,
                            block,
                            index,
                        };
                        main_to_net_tx.send(message).await;
                    }
                    ProtocolMessage::BlockResponse { block } => {
                        // validate block
                        if !block.is_valid() {
                            panic!("Received invalid block response while syncing the chain");
                            // later, should request from another peer and black list this peer
                        }

                        match ledger.apply_block_by_id(&block) {
                            BlockStatus::Applied => {
                                // may still need to skip the loop before solving, in case any more pending blocks have queued

                                info!("Applied new block received over the network during sync to the ledger");

                                let block_id = block.get_id();

                                // check to see if this block is the parent referenced by any cached blocks
                                if ledger.is_pending_parent(&block_id) {
                                    // fetch the pending full block that references this parent, will call recursive
                                    // let child_id = ledger.get_child_of_pending_parent(&block_id);
                                    let challenge = ledger.apply_pending_block(block_id);
                                    info!("Synced the ledger!");

                                    if node_type == NodeType::Farmer
                                        || node_type == NodeType::Gateway
                                    {
                                        main_to_sol_tx
                                            .send(ProtocolMessage::BlockChallenge { challenge })
                                            .await;
                                    }

                                    continue;
                                }

                                // if not then request block at the next index
                                main_to_net_tx
                                    .send(ProtocolMessage::BlockRequest {
                                        index: ledger.height,
                                    })
                                    .await;
                            }
                            BlockStatus::Pending => {
                                // error in sequencing
                                panic!("Should not be receiving pending blocks as responses during block sync process!");
                            }
                            BlockStatus::Invalid => {
                                panic!("Should not be applying blocks out of order during block sync process!");
                            }
                        }
                    }
                    ProtocolMessage::BlockProposalRemote {
                        full_block,
                        peer_addr,
                    } => {
                        let block_id = full_block.block.get_id();

                        // do you already have this block?
                        if ledger.is_block_applied(&block_id) {
                            info!("Received a block proposal via gossip for known block, ignoring");
                            continue;
                        }

                        // do you have the parent?
                        if !ledger.is_block_applied(&full_block.block.parent_id) {
                            // cache the block until fully synced
                            // either: still syncing the ledger from startup or received recent blocks out of order
                            info!("Caching a block proposal that is ahead of local ledger with id: {}", hex::encode(&full_block.block.get_id()[0..8]));
                            ledger.cache_pending_block(full_block);
                            continue;
                        }

                        // check quality first
                        if full_block.block.get_quality() < ledger.quality_threshold {
                            info!("Received block proposal with insufficient quality via gossip, ignoring");
                            continue;
                        }

                        // make sure the proof is correct
                        if !full_block.is_valid(
                            PLOT_SIZE,
                            &ledger.merkle_root,
                            &genesis_piece_hash,
                            &ledger.sloth,
                        ) {
                            info!("Received invalid block proposal via gossip, ignoring");
                            continue;
                        }

                        // now we can finally apply the block
                        match ledger.apply_block_by_id(&full_block.block) {
                            BlockStatus::Applied => {
                                info!("Applied new block received over the network via gossip to the ledger");

                                // is this the parent of a pending block?
                                if ledger.is_pending_parent(&block_id) {
                                    let challenge = ledger.apply_pending_block(block_id);

                                    if node_type == NodeType::Farmer || node_type == NodeType::Gateway {
                                        main_to_sol_tx.send(ProtocolMessage::BlockChallenge { challenge }).await;
                                    }

                                    continue;
                                }

                                // else gossip
                                main_to_net_tx.send(ProtocolMessage::BlockProposalRemote { full_block: full_block.clone(), peer_addr }).await;

                                // solve if farming
                                if node_type == NodeType::Farmer || node_type == NodeType::Gateway {
                                    main_to_sol_tx.send(ProtocolMessage::BlockChallenge { challenge: full_block.block.get_id() }).await;
                                }
                            }
                            BlockStatus::Pending => {
                                panic!("Logic error, add_block_by_id should not have been called if the parent is unknown...")
                            }
                            BlockStatus::Invalid => {
                                info!("Could not apply block to ledger, illegal extension...");
                            }
                        }
                    }
                    ProtocolMessage::BlockSolution { solution } => {
                        info!(
                            "Received a solution for challenge: {}",
                            hex::encode(&solution.challenge[0..8])
                        );
                        // check solution quality is high enough
                        if solution.quality < ledger.quality_threshold {
                            info!("Solution to block challenge does not meet quality threshold, ignoring");
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
                                info!("Applied new block generated locally to the ledger!");
                                // block.print();

                                let proof = Proof::new(
                                    solution.encoding,
                                    crypto::get_merkle_proof(solution.index, &merkle_proofs),
                                    solution.index,
                                );

                                let solve = main_to_sol_tx.send(ProtocolMessage::BlockChallenge {
                                    challenge: block.get_id(),
                                });
                                let gossip =
                                    main_to_net_tx.send(ProtocolMessage::BlockProposalLocal {
                                        full_block: FullBlock { block, proof },
                                    });

                                join!(solve, gossip);
                            }
                            BlockStatus::Invalid => {
                                // illegal extension to the ledger, ignore
                                // may have applied a better block received over the network while the solution was being generated
                                info!("Attempted to add locally generated block to the ledger, but was no longer valid");
                            }
                            BlockStatus::Pending => {
                                // this should not happen, control flow logic error
                                panic!("A block generated locally does not have a known parent...")
                            }
                        }
                    }
                    ProtocolMessage::StateUpdateResponse { mut state } => {
                        state.node_type = node_type.to_string();
                        state.peers = state.peers + "/" + &MAX_PEERS.to_string()[..];
                        state.blocks = ledger.get_block_height().to_string();
                        state.pieces = match node_type {
                            NodeType::Gateway => PLOT_SIZE.to_string(),
                            NodeType::Farmer => PLOT_SIZE.to_string(),
                            NodeType::Peer => 0.to_string(),
                        };
                        state_sender.send(state).unwrap();
                    }
                    _ => panic!("Main protocol listener has received an unknown protocol message!"),
                }
            }
        }
    };

    let protocol_startup = async {
        info!("Calling protocol startup");

        match node_type {
            NodeType::Gateway => {
                // send genesis challenge to solver
                // this will start an eval loop = solve -> create block -> gossip -> solve ...
                info!(
                    "Starting gateway with genesis challenge: {}",
                    hex::encode(&genesis_piece_hash[0..8])
                );
                main_to_sol_tx
                    .send(ProtocolMessage::BlockChallenge {
                        challenge: genesis_piece_hash,
                    })
                    .await;
            }
            NodeType::Peer | NodeType::Farmer => {
                // start syncing the ledger at the genesis block
                // this will start a sync loop that should complete when fully synced
                // at that point node will simply listen and solve
                task::sleep(Duration::from_secs(1)).await;
                info!("New peer starting ledger sync with gateway");
                main_to_net_tx
                    .send(ProtocolMessage::BlockRequest { index: 0 })
                    .await;
            }
        }

        // send state update requests in a loop to network
        loop {
            main_to_net_tx
                .send(ProtocolMessage::StateUpdateRequest)
                .await;
            task::sleep(Duration::from_millis(1000)).await;
        }
    };

    join!(protocol_listener, protocol_startup);
}
