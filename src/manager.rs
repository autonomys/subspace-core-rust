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
use std::fmt;
use std::fmt::Display;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/*
 * Consider if default should wait for block arrival before collecting tx and gossping or release immeaditely?
 * Maybe rename timestamp to arrival_time
 *
 * Deadlines
 * ---------
 * 1) I have solved locally, forge and gossip immediately, wait for deadline before applying and solving on top of (farmer)
 * 2) I have received a block, validate and gossip immediately, wait for deadline before applying to the ledger (peer)
 * 3) I have received a block, validate and gossip immediately, wait for deadline before allying and building (farmer)
 *
 * Farmers will attempt to build on a valid block once it arrives
 * They should then immeadiatley fill with tx and gossip the block (rational behavior)
 *
 *
 *
*/

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
        base_time: u128,
        is_genesis: bool,
    },
    /// Solver sends a set of solutions back to main for application
    BlockSolutions {
        solutions: Vec<Solution>,
    },
    BlockArrived {
        block: Block,
    },
    /// Main sends a state update request to manager for console state
    StateUpdateRequest,
    /// Manager sends a state update response to main for console state
    StateUpdateResponse {
        state: AppState,
    },
}

impl Display for ProtocolMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::BlockRequest { .. } => "BlockRequest",
                Self::BlockResponse { .. } => "BlockResponse",
                Self::BlockRequestFrom { .. } => "BlockRequestFrom",
                Self::BlockResponseTo { .. } => "BlockResponseTo",
                Self::BlockProposalRemote { .. } => "BlockProposalRemote",
                Self::BlockProposalLocal { .. } => "BlockProposalLocal",
                Self::BlockChallenge { .. } => "BlockChallenge",
                Self::BlockSolutions { .. } => "BlockSolutions",
                Self::BlockArrived { .. } => "BlockArrived",
                Self::StateUpdateRequest { .. } => "StateUpdateRequest",
                Self::StateUpdateResponse { .. } => "StateUpdateResponse",
            }
        )
    }
}

/// Spawns a non-blocking task that will wait for the block arrival based on local time
pub async fn wait_for_block_arrival(block: Block, sender: &Sender<ProtocolMessage>) {
    let cloned_sender = sender.clone();
    async_std::task::spawn(async move {
        let time_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        if time_now < block.timestamp {
            async_std::task::sleep(Duration::from_millis((block.timestamp - time_now) as u64))
                .await;
        };

        cloned_sender
            .send(ProtocolMessage::BlockArrived { block })
            .await;
    });
}

/// Starts the manager process, a broker loop that acts as the central async message hub for the node
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
    main_to_main_tx: Sender<ProtocolMessage>,
    state_sender: crossbeam_channel::Sender<AppState>,
) {
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
                        // TODO: this is only a partial block, we have to validate some full blocks too
                        if !block.is_valid() {
                            // TODO: Request from another peer and blacklist this peer
                            panic!("Received invalid block response while syncing the chain");
                        }

                        match ledger.apply_block_by_id(&block) {
                            BlockStatus::Confirmed => {
                                // may still need to skip the loop before solving, in case any more pending blocks have queued

                                info!("Applied new block received over the network during sync to the ledger");

                                let block_id = block.get_id();

                                // check to see if this block is the parent referenced by any cached blocks
                                if ledger.is_pending_parent(&block_id) {
                                    // fetch the pending full block that references this parent, will call recursive

                                    // TODO: should be an async background task, but can't because it will move the ledger
                                    // Change pattern to communicate with ledger async over channel or through mutex
                                    // then change apply_pending_children back to async fn that calls wait_for_block_arrival()
                                    let (challenge, timestamp) =
                                        ledger.apply_pending_children(block_id);
                                    info!("Synced the ledger!");

                                    if node_type == NodeType::Farmer
                                        || node_type == NodeType::Gateway
                                    {
                                        main_to_sol_tx
                                            .send(ProtocolMessage::BlockChallenge {
                                                challenge,
                                                base_time: timestamp,
                                                is_genesis: false,
                                            })
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

                        if !ledger.is_block_applied(&full_block.block.parent_id) {
                            // cache the block until fully synced
                            // either: still syncing the ledger from startup or received recent blocks out of order
                            info!("Caching a block proposal that is ahead of local ledger with id: {}", hex::encode(&full_block.block.get_id()[0..8]));
                            ledger.cache_pending_block(full_block);
                            continue;
                        }

                        // TODO: revise for delay
                        // check quality first
                        // if full_block.block.get_quality() < ledger.quality_threshold {
                        //     info!("Received block proposal with insufficient quality via gossip, ignoring");
                        //     continue;
                        // }

                        // make sure the proof is correct
                        if !full_block.is_valid(
                            &ledger.merkle_root,
                            &genesis_piece_hash,
                            &ledger.sloth,
                        ) {
                            info!("Received invalid block proposal via gossip, ignoring");
                            continue;
                        }

                        // TODO: decide to gossip the block now or wait until it has arrived?
                        // for now gossip optimisitically
                        main_to_net_tx
                            .send(ProtocolMessage::BlockProposalRemote {
                                full_block: full_block.clone(),
                                peer_addr,
                            })
                            .await;

                        // wait for deadline arrival
                        wait_for_block_arrival(full_block.block, &main_to_main_tx).await;
                    }
                    ProtocolMessage::BlockSolutions { solutions } => {
                        // info!(
                        //     "Received solutions for challenge: {}",
                        //     hex::encode(&solutions[0].challenge[0..8])
                        // );

                        for solution in solutions.into_iter() {
                            // info!("Received a solution with delay: {}", solution.delay);

                            // immediately gossip, on arrival apply, solve if valid extension

                            // TODO: once we have enough farmers, ensure that delays > 1 second are ignored
                            // if solution.delay < 1000u32 {
                            //     info!("Solution to block challenge has a delay greater than one second, ignoring");
                            //     continue;
                            // }

                            let timestamp = solution.base_time + solution.delay as u128;
                            // sign tag and create block
                            let block = Block::new(
                                timestamp,
                                solution.delay,
                                solution.challenge,
                                solution.tag,
                                binary_public_key,
                                keys.sign(&solution.tag).to_bytes().to_vec(),
                                tx_payload.clone(),
                            );

                            // create the proof
                            let proof = Proof::new(
                                solution.encoding,
                                crypto::get_merkle_proof(solution.index, &merkle_proofs),
                                solution.index,
                            );

                            // gossip the block immediately
                            main_to_net_tx
                                .send(ProtocolMessage::BlockProposalLocal {
                                    full_block: FullBlock {
                                        block: block.clone(),
                                        proof,
                                    },
                                })
                                .await;

                            // wait for deadline arrival
                            wait_for_block_arrival(block, &main_to_main_tx).await;
                        }
                    }
                    ProtocolMessage::BlockArrived { block } => {
                        // TODO: ensure that we do not apply stale blocks to the ledger
                        // info!("A new block has arrived");

                        match ledger.apply_recent_block(&block) {
                            BlockStatus::Confirmed => {
                                let block_id = block.get_id();
                                let challenge = block_id;
                                let base_time = block.timestamp;

                                // TODO: Handle the potential for cached blocks waiting for a parents arrival
                                // if ledger.is_pending_parent(&block_id) {
                                //     let (last_challenge, last_base_time) =
                                //         ledger.apply_pending_children(block_id);
                                //     challenge = last_challenge;
                                //     base_time = last_base_time;
                                // }

                                if node_type == NodeType::Farmer || node_type == NodeType::Gateway {
                                    // now we solve

                                    // info!("Sending challenge to solver");

                                    main_to_sol_tx
                                        .send(ProtocolMessage::BlockChallenge {
                                            challenge,
                                            base_time,
                                            is_genesis: false,
                                        })
                                        .await;
                                }
                            }
                            BlockStatus::Pending => {}
                            BlockStatus::Invalid => {}
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

                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_millis();

                main_to_sol_tx
                    .send(ProtocolMessage::BlockChallenge {
                        challenge: genesis_piece_hash,
                        base_time: timestamp,
                        is_genesis: true,
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
        if CONSOLE {
            loop {
                main_to_net_tx
                    .send(ProtocolMessage::StateUpdateRequest)
                    .await;
                task::sleep(Duration::from_millis(1000)).await;
            }
        }
    };

    join!(protocol_listener, protocol_startup);
}
