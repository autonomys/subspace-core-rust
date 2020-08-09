#![allow(dead_code)]

use super::*;
use async_std::sync::{Receiver, Sender};
use async_std::task;
use console::AppState;
use futures::join;
use ledger::{Block, BlockState, FullBlock, Proof};
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
 * Simplest way to handle sync
 *
 * Cache interim gossip an apply after sync (though we may miss some)
 * Have them sync all gossip after sync, then start listening to gossip
 * How do we send gossip again?
 * Have to get all blocks that have not been applied
 * But theyre all in the same data structure
 * Could just read children down from last confirmed block
 * This would miss blocks that have not arrived
 * But these should be received via gossip if we apply them on arrival
 *
 * Maybe we should think of it more like pub/sub
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
        index: u32,
        block: Option<Block>,
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
        parent_id: [u8; 32],
        challenge: [u8; 32],
        base_time: u128,
    },
    /// Solver sends a set of solutions back to main for application
    BlockSolutions {
        solutions: Vec<Solution>,
    },
    BlockArrived {
        block_id: [u8; 32],
        cached: bool,
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
pub async fn wait_for_block_arrival(
    block_id: [u8; 32],
    cached: bool,
    timestamp: u128,
    sender: &Sender<ProtocolMessage>,
) {
    let cloned_sender = sender.clone();
    async_std::task::spawn(async move {
        let time_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        if time_now < timestamp {
            async_std::task::sleep(Duration::from_millis((timestamp - time_now) as u64)).await;
        };

        cloned_sender
            .send(ProtocolMessage::BlockArrived { block_id, cached })
            .await;
    });
}

pub async fn arrive_pending_children(
    ledger: &ledger::Ledger,
    children: Vec<[u8; 32]>,
    sender: &Sender<ProtocolMessage>,
) {
    info!("Calling arrive_pending_children");
    for child_block_id in children.iter() {
        let child_block = ledger.metablocks.get(child_block_id).unwrap();

        // check if arrival has expired, await if needed
        // this has to occur in the background
        // need a different listener to handle arrival

        // when those arrive we see if their children are in pending children for parent
        // if yes repeat
        // otherewise call normal wait for arrival

        wait_for_block_arrival(*child_block_id, true, child_block.block.timestamp, &sender).await;

        // dont want to solve until we are no longer calling cached blocks
        // once we are done calling cached blocks, we need to clear out the rest of them
    }
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
    // create the genesis block here
    let genesis_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_millis();

    // sign tag and create block
    let genesis_block = Block::new(
        genesis_timestamp,
        0u32,
        [0u8; 32],
        [0u8; 32],
        genesis_piece_hash,
        binary_public_key,
        keys.sign(&genesis_piece_hash).to_bytes().to_vec(),
        tx_payload.clone(),
    );

    let genesis_block_id = genesis_block.get_id();

    let protocol_listener = async {
        info!("Main protocol loop is running...");
        ledger.apply_genesis_block(&genesis_block);
        info!(
            "Applied the genesis block to ledger with id {}",
            hex::encode(&genesis_block_id)
        );

        loop {
            if let Some(message) = any_to_main_rx.recv().await.ok() {
                match message {
                    ProtocolMessage::BlockRequestFrom { node_addr, index } => {
                        // TODO: should send a unique message if block has no children to signify that this is the last block
                        let block = ledger.get_block_by_index(index);
                        let message = ProtocolMessage::BlockResponseTo {
                            node_addr,
                            block,
                            index,
                        };
                        main_to_net_tx.send(message).await;
                    }
                    ProtocolMessage::BlockResponse { block, index } => {
                        match block {
                            Some(block) => {
                                // apply the block, go to the next index

                                // TODO: this is only a partial block, we have to validate some full blocks too
                                if !block.is_valid() {
                                    // TODO: Request from another peer and blacklist this peer
                                    panic!(
                                        "Received invalid block response while syncing the chain"
                                    );
                                }

                                // TODO: check timestamp has elapsed

                                if index == 0 {
                                    ledger.apply_genesis_block(&block);
                                } else {
                                    ledger.track_block(&block, BlockState::New);
                                    ledger.confirm_block(&block.get_id());
                                }

                                // request another block
                                main_to_net_tx
                                    .send(ProtocolMessage::BlockRequest {
                                        index: ledger.height,
                                    })
                                    .await;
                            }
                            None => {
                                // we have synced, start applying pending blocks from the last index
                                info!("Synced the confirmed ledger!");

                                let children = ledger
                                    .pending_children_for_parent
                                    .get(&ledger.latest_block_hash)
                                    .unwrap();

                                arrive_pending_children(ledger, children.clone(), &main_to_main_tx)
                                    .await;
                            }
                        }
                    }
                    ProtocolMessage::BlockProposalRemote {
                        full_block,
                        peer_addr,
                    } => {
                        let block_id = full_block.block.get_id();

                        if ledger.metablocks.contains_key(&block_id) {
                            info!("Received a block proposal via gossip for known block, ignoring");
                            continue;
                        }

                        // TODO: revise for delay
                        // check quality first
                        // if full_block.block.get_quality() < ledger.quality_threshold {
                        //     info!("Received block proposal with insufficient quality via gossip, ignoring");
                        //     continue;
                        // }

                        // TODO: Should check if block is pending parent for children here

                        match ledger.metablocks.get(&full_block.block.parent_id) {
                            Some(block) => match block.state {
                                BlockState::New | BlockState::Arrived | BlockState::Confirmed => {
                                    // TODO: decide to gossip the block now or wait until it has arrived?
                                    // for now gossip optimisitically
                                    main_to_net_tx
                                        .send(ProtocolMessage::BlockProposalRemote {
                                            full_block: full_block.clone(),
                                            peer_addr,
                                        })
                                        .await;
                                    info!(
                                        "Waiting for arrival of new block receieved via gossip: {}",
                                        hex::encode(&block.id[0..8])
                                    );

                                    // Add to block tracker
                                    ledger.track_block(&full_block.block, BlockState::New);

                                    // wait for deadline arrival
                                    wait_for_block_arrival(
                                        full_block.block.get_id(),
                                        false,
                                        full_block.block.timestamp,
                                        &main_to_main_tx,
                                    )
                                    .await;
                                    continue;
                                }
                                _ => {}
                            },
                            None => {}
                        }

                        // State is stray or no parent is tracked
                        info!(
                            "Caching a block proposal that is ahead of local ledger with id: {}",
                            hex::encode(&full_block.block.get_id()[0..8])
                        );
                        ledger.track_block(&full_block.block, BlockState::Stray);
                        ledger
                            .pending_children_for_parent
                            .entry(full_block.block.parent_id)
                            .and_modify(|children| children.push(block_id))
                            .or_insert(vec![block_id]);

                        // TODO: What does this code do???
                        if !ledger.metablocks.contains_key(&full_block.block.parent_id) {
                            continue;
                        }
                    }
                    ProtocolMessage::BlockSolutions { solutions } => {
                        for solution in solutions.into_iter() {
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
                                solution.parent,
                                solution.tag,
                                binary_public_key,
                                keys.sign(&solution.tag).to_bytes().to_vec(),
                                tx_payload.clone(),
                            );

                            // create the proof
                            let proof = Proof::new(
                                solution.encoding,
                                crypto::get_merkle_proof(solution.proof_index, &merkle_proofs),
                                solution.piece_index,
                                solution.proof_index,
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

                            // Add to block tracker
                            ledger.track_block(&block, BlockState::New);

                            // wait for deadline arrival
                            wait_for_block_arrival(
                                block.get_id(),
                                false,
                                block.timestamp,
                                &main_to_main_tx,
                            )
                            .await;
                        }
                    }
                    ProtocolMessage::BlockArrived { block_id, cached } => {
                        // TODO: ensure that we do not apply stale blocks to the ledger
                        // info!(
                        //     "A new block has arrived with id: {}",
                        //     hex::encode(&block_id[0..8])
                        // );

                        ledger.apply_arrived_block(&block_id, &main_to_sol_tx).await;

                        // ToDo: Have to wipe cached blocks at some point to prevent memory leak

                        if cached {
                            // block was cached and has arrived on sync
                            // check for more cached pending children
                            match ledger.pending_children_for_parent.get(&block_id) {
                                Some(children) => {
                                    arrive_pending_children(
                                        ledger,
                                        children.clone(),
                                        &main_to_main_tx,
                                    )
                                    .await;
                                }
                                None => {}
                            }
                        }

                        // have to tell it the node type
                        // and whether to solve or not
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

                // add the block to the ledger

                // should create the genesis block here then apply it and solve

                main_to_sol_tx
                    .send(ProtocolMessage::BlockChallenge {
                        parent_id: genesis_block_id,
                        challenge: genesis_piece_hash,
                        base_time: genesis_timestamp,
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
