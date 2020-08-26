#![allow(dead_code)]

use super::*;
use async_std::sync::{Receiver, Sender};
use async_std::task;
use console::AppState;
use futures::join;
use ledger::{Block, BlockState};
use log::*;
use network::NodeType;
use solver::Solution;
use std::fmt;
use std::fmt::Display;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/*
 * Sync Workflow
 *
 * Peer requests block at height 0
 * GW sends block 0
 * Peer checks to see if has any cached blocks that reference the block received
 * If yes, peer recursively applies cached blocks
 * If no, peer requests the next block
 *
 * Need to pull old code for apply_pending_chlildren
 * Need to complete the code when blocks are received ahead of the current timeslot (wait for arrival)
 *
*/

// TODO: Split this into multiple enums
pub enum ProtocolMessage {
    /// On sync, main forwards block request to Net for tx from self to peer
    BlocksRequest {
        timeslot: u64,
    },
    /// peer receives at Net and forwards request to main for fetch
    BlocksRequestFrom {
        node_addr: SocketAddr,
        timeslot: u64,
    },
    /// peer main forwards response back to Net for tx
    BlocksResponseTo {
        node_addr: SocketAddr,
        blocks: Vec<Block>,
        timeslot: u64,
    },
    /// self receives at Net and forwards response back to Main
    BlocksResponse {
        timeslot: u64,
        blocks: Vec<Block>,
    },
    /// Net receives new full block, validates/applies, sends back to net for re-gossip
    BlockProposalRemote {
        block: Block,
        peer_addr: SocketAddr,
    },
    /// A valid full block has been produced locally and needs to be gossiped
    BlockProposalLocal {
        block: Block,
    },
    /// Main sends challenge to solver for evaluation
    SlotChallenge {
        epoch: u64,
        timeslot: u64,
        epoch_randomness: [u8; 32],
        slot_challenge: [u8; 32],
    },
    /// Solver sends a set of solutions back to main for application
    BlockSolutions {
        solutions: Vec<Solution>,
    },
    BlockArrived {
        block_id: [u8; 32],
        cached: bool,
    },
    StartFarming,
    StopFarming,
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
                Self::BlocksRequest { .. } => "BlockRequest",
                Self::BlocksResponse { .. } => "BlockResponse",
                Self::BlocksRequestFrom { .. } => "BlockRequestFrom",
                Self::BlocksResponseTo { .. } => "BlockResponseTo",
                Self::BlockProposalRemote { .. } => "BlockProposalRemote",
                Self::BlockProposalLocal { .. } => "BlockProposalLocal",
                Self::SlotChallenge { .. } => "BlockChallenge",
                Self::BlockSolutions { .. } => "BlockSolutions",
                Self::BlockArrived { .. } => "BlockArrived",
                Self::StartFarming => "StartFarming",
                Self::StopFarming => "StopFarming",
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
    sender: Sender<ProtocolMessage>,
) {
    async_std::task::spawn(async move {
        let time_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        if time_now < timestamp {
            async_std::task::sleep(Duration::from_millis((timestamp - time_now) as u64)).await;
        }

        sender
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

        // wait_for_block_arrival(
        //     *child_block_id,
        //     true,
        //     child_block.block.timestamp,
        //     sender.clone(),
        // )
        // .await;

        // dont want to solve until we are no longer calling cached blocks
        // once we are done calling cached blocks, we need to clear out the rest of them
    }
}

/// Starts the manager process, a broker loop that acts as the central async message hub for the node
pub async fn run(
    node_type: NodeType,
    genesis_piece_hash: [u8; 32],
    ledger: &mut ledger::Ledger,
    any_to_main_rx: Receiver<ProtocolMessage>,
    main_to_net_tx: Sender<ProtocolMessage>,
    main_to_main_tx: Sender<ProtocolMessage>,
    state_sender: crossbeam_channel::Sender<AppState>,
    timer_to_solver_tx: Sender<ProtocolMessage>,
    epoch_tracker: timer::EpochTracker,
) {
    let protocol_listener = async {
        info!("Main protocol loop is running...");
        let is_farming = match node_type {
            NodeType::Gateway | NodeType::Farmer => true,
            _ => false,
        };

        // if gateway init the genesis block set and then start the timer
        if node_type == NodeType::Gateway {
            // init ledger from genesis
            ledger.init_from_genesis().await;

            // TODO: are we sure this is starting at the exaxt right time?
            // start timer loop
            let sender = timer_to_solver_tx.clone();
            let tracker = Arc::clone(&epoch_tracker);
            async_std::task::spawn(async move {
                timer::run(
                    sender,
                    tracker,
                    CHALLENGE_LOOKBACK,
                    CHALLENGE_LOOKBACK * TIMESLOTS_PER_EPOCH,
                    true,
                )
                .await;
            });
        } else {
            // create the initial epoch
            let initial_epoch = timer::Epoch::new(0);
            epoch_tracker.lock().await.insert(0, initial_epoch);
        }

        loop {
            if let Ok(message) = any_to_main_rx.recv().await {
                match message {
                    ProtocolMessage::BlocksRequestFrom {
                        node_addr,
                        timeslot,
                    } => {
                        // TODO: should send a unique message if block has no children to signify that this is the last block
                        let blocks = ledger.get_blocks_by_timeslot(timeslot);
                        let message = ProtocolMessage::BlocksResponseTo {
                            node_addr,
                            blocks,
                            timeslot,
                        };
                        main_to_net_tx.send(message).await;
                    }
                    ProtocolMessage::BlocksResponse { blocks, timeslot } => {
                        // TODO: this is mainly for testing, later this will be replaced by state chain sync
                        // as such there is no need for validating the block or timestamp
                        // determine if genesis block or normal block
                        // apply genesis block
                        // apply normal block

                        // TODO: sort the blocks lexicographically

                        // get the epoch for this round
                        let mut epoch = epoch_tracker
                            .lock()
                            .await
                            .get(&(timeslot / TIMESLOTS_PER_EPOCH))
                            .unwrap()
                            .clone();

                        // apply each block for the timeslot
                        for block in blocks.iter() {
                            ledger.apply_block_from_sync(block.clone()).await;
                        }

                        // increment the timeslot
                        ledger.current_timeslot += 1;

                        // increment the epoch on boundary
                        if ledger.current_timeslot % TIMESLOTS_PER_EPOCH == 0 {
                            ledger.current_epoch += 1;

                            // close the current epoch here and derive randomness
                            epoch.close();

                            // create the new epoch
                            let new_epoch_index = timeslot / TIMESLOTS_PER_EPOCH;
                            epoch_tracker
                                .lock()
                                .await
                                .insert(new_epoch_index, timer::Epoch::new(new_epoch_index));
                        }

                        // we want to ensure that all blocks in this timeslot have pending children (else we may have only a partial set)
                        let mut synced = true;
                        let block_ids: Vec<BlockId> =
                            blocks.iter().map(|block| block.get_id()).collect();

                        for block_id in block_ids.iter() {
                            match ledger.pending_children_for_parent.get(block_id) {
                                Some(_) => {}
                                None => {
                                    synced = false;
                                    break;
                                }
                            }
                        }

                        if synced {
                            // if so, then recursively apply all gossip, until we run out of cached blocks

                            // then start the timer

                            let sender = timer_to_solver_tx.clone();
                            let tracker = Arc::clone(&epoch_tracker);

                            // apply the genesis block

                            // WRONG: We can't start the timer until

                            // start the timer at the right epoch and timeslot
                            let genesis_block = blocks[0].clone();
                            let genesis_time = genesis_block.content.timestamp;

                            let current_time = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .expect("Time went backwards")
                                .as_millis();

                            let elapsed_time = current_time - genesis_time;
                            let mut elapsed_timeslots = elapsed_time / TIMESLOT_DURATION;
                            let mut elapsed_epochs =
                                elapsed_timeslots / TIMESLOTS_PER_EPOCH as u128;
                            let time_to_next_timeslot =
                                elapsed_time - (TIMESLOT_DURATION * elapsed_timeslots);

                            // TODO: if time_to_next_timeslot is very small (say less than 10 ms) wait one more timeslot to ensure computation completes

                            async_std::task::sleep(Duration::from_millis(
                                (time_to_next_timeslot) as u64,
                            ))
                            .await;

                            elapsed_timeslots += 1;

                            if elapsed_timeslots % TIMESLOTS_PER_EPOCH as u128 == 0 {
                                elapsed_epochs += 1;
                            }
                            async_std::task::spawn(async move {
                                timer::run(
                                    sender,
                                    tracker,
                                    elapsed_epochs as u64,
                                    elapsed_timeslots as u64,
                                    is_farming,
                                )
                                .await;
                            });

                        // then start farming
                        } else {
                            main_to_net_tx
                                .send(ProtocolMessage::BlocksRequest {
                                    timeslot: timeslot + 1,
                                })
                                .await;
                        }
                    }
                    ProtocolMessage::BlockProposalRemote { block, peer_addr } => {
                        let block_id = block.get_id();

                        if ledger.metablocks.contains_key(&block_id) {
                            info!("Received a block proposal via gossip for known block, ignoring");
                            continue;
                        }

                        // only apply if we have the parent, else cache
                        // apply if we have one or all of the parents

                        for parent_id in block.content.parent_ids.iter() {
                            if !ledger.metablocks.contains_key(parent_id) {
                                // cache the block
                                // ledger.metablocks.insert();
                                // add to pending children
                            }
                        }

                        // TODO: if the timeslot is beyond the grace period, ignore the block
                        // TODO: important -- this may lead to forks if nodes are malicous
                        // TODO: this means we need to support forks in some fashion

                        // has the timeslot arrived? -> case where a node gossips early intentionally or clocks are out of sync
                        let time_now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .expect("Time went backwards")
                            .as_millis();

                        let arrival_time = ledger.genesis_timestamp
                            + (TIMESLOT_DURATION * block.proof.timeslot as u128);

                        if arrival_time < time_now {
                            let sender = main_to_main_tx.clone();
                            async_std::task::spawn(async move {
                                async_std::task::sleep(Duration::from_millis(
                                    (arrival_time - time_now) as u64,
                                ))
                                .await;
                                sender
                                    .send(ProtocolMessage::BlockArrived {
                                        block_id,
                                        cached: false,
                                    })
                                    .await;
                            });
                        }

                        // how do we know if we have to cache the block (if received during sync?)

                        // check if the block is valid and apply
                        if ledger.validate_and_apply_remote_block(block.clone()).await {
                            main_to_net_tx
                                .send(ProtocolMessage::BlockProposalRemote {
                                    block: block,
                                    peer_addr,
                                })
                                .await;
                        }
                    }
                    ProtocolMessage::BlockArrived { block_id, cached } => {
                        info!(
                            "A new block has arrived with id: {}",
                            hex::encode(&block_id[0..8])
                        );

                        // ledger.apply_arrived_block(&block_id, &main_to_sol_tx).await;

                        // ToDo: Have to wipe cached blocks at some point to prevent memory leak

                        if cached {
                            // block was cached and has arrived on sync
                            // check for more cached pending children
                            if let Some(children) =
                                ledger.pending_children_for_parent.get(&block_id)
                            {
                                arrive_pending_children(ledger, children.clone(), &main_to_main_tx)
                                    .await;
                            }
                        }

                        // have to tell it the node type
                        // and whether to solve or not
                    }
                    ProtocolMessage::BlockSolutions { solutions } => {
                        // TODO: split into two functions
                        if solutions.is_empty() {
                            ledger.create_and_apply_local_block(None).await;
                        } else {
                            for solution in solutions.into_iter() {
                                let block = ledger
                                    .create_and_apply_local_block(Some(solution))
                                    .await
                                    .unwrap();
                                main_to_net_tx
                                    .send(ProtocolMessage::BlockProposalLocal {
                                        block: block.clone(),
                                    })
                                    .await;
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
                    "Starting gateway with genesis epoch challenge: {}",
                    hex::encode(&genesis_piece_hash[0..8])
                );
            }
            NodeType::Peer | NodeType::Farmer => {
                // start syncing the ledger at the genesis block
                // this will start a sync loop that should complete when fully synced
                // at that point node will simply listen and solve
                task::sleep(Duration::from_secs(1)).await;
                info!("New peer starting ledger sync with gateway");
                main_to_net_tx
                    .send(ProtocolMessage::BlocksRequest { timeslot: 0 })
                    .await;

                // on genesis block

                // init the timer
            }
        };

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
