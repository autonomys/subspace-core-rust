use crate::block::Block;
use crate::console::AppState;
use crate::farmer::{FarmerMessage, Solution};
use crate::ledger::Ledger;
use crate::network::messages::{
    BlocksRequest, BlocksResponse, GossipMessage, RequestMessage, ResponseMessage,
};
use crate::network::{Network, NodeType};
use crate::timer::EpochTracker;
use crate::transaction::Transaction;
use crate::{
    timer, CHALLENGE_LOOKBACK_EPOCHS, CONSOLE, EPOCH_GRACE_PERIOD, MIN_PEERS, PLOT_SIZE,
    TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION,
};
use async_std::sync::{Receiver, Sender};
use async_std::task;
use futures::join;
use futures::lock::Mutex;
use log::*;
use std::fmt;
use std::fmt::Display;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/*
 * Sync Workflow
 *
 * Peer requests block at height 0
 * GW sends block 0
 * Peer checks to see if has any cached blocks that reference the block received
 * If yes, peer recursively applies cached blocks
 * If no, peer requests the next block
*/

// TODO: Split this into multiple enums
pub enum ProtocolMessage {
    /// Solver sends a set of solutions back to main for application
    BlockSolutions { solutions: Vec<Solution> },
    BlockArrived {
        block: Block,
        peer_addr: SocketAddr,
        cached: bool,
    },
}

impl Display for ProtocolMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::BlockSolutions { .. } => "BlockSolutions",
                Self::BlockArrived { .. } => "BlockArrived",
            }
        )
    }
}

pub type SharedLedger = Arc<Mutex<Ledger>>;

/// start the timer after syncing the ledger
pub fn start_timer(
    timer_to_farmer_tx: Sender<FarmerMessage>,
    is_farming: bool,
    epoch_tracker: EpochTracker,
    genesis_timestamp: u64,
    next_timeslot: u64,
    ledger: SharedLedger,
) {
    async_std::task::spawn(timer::run(
        timer_to_farmer_tx,
        epoch_tracker,
        is_farming,
        genesis_timestamp,
        next_timeslot,
        ledger,
    ));
}

/// Starts the manager process, a broker loop that acts as the central async message hub for the node
pub async fn run(
    node_type: NodeType,
    genesis_piece_hash: [u8; 32],
    ledger: Ledger,
    any_to_main_rx: Receiver<ProtocolMessage>,
    network: Network,
    main_to_main_tx: Sender<ProtocolMessage>,
    state_sender: crossbeam_channel::Sender<AppState>,
    timer_to_farmer_tx: Sender<FarmerMessage>,
    epoch_tracker: EpochTracker,
) {
    let ledger: SharedLedger = Arc::new(Mutex::new(ledger));
    {
        let network = network.clone();
        let epoch_tracker = epoch_tracker.clone();
        let ledger = Arc::clone(&ledger);

        async_std::task::spawn(async move {
            let gossip_receiver = network.get_gossip_receiver().unwrap();
            while let Ok((peer_addr, message)) = gossip_receiver.recv().await {
                match message {
                    GossipMessage::BlockProposal { block } => {
                        let mut ledger = ledger.lock().await;
                        trace!(
                            "Received a block via gossip, with {} uncles",
                            block.content.uncle_ids.len()
                        );

                        // TODO: need to reference block by proof not by full block
                        let proof_id = block.proof.get_id();

                        if ledger.metablocks.contains_key(&proof_id) {
                            warn!("Received a block proposal via gossip for known block, ignoring");
                            continue;
                        }

                        if !ledger.timer_is_running {
                            trace!(
                                "Caching a block received via gossip before the ledger is synced"
                            );
                            ledger.cache_remote_block(block);
                            continue;
                        }

                        // TODO: this should be set once as a constant on ledger
                        let genesis_instant = Instant::now()
                            - (UNIX_EPOCH.elapsed().unwrap()
                                - Duration::from_millis(ledger.genesis_timestamp));

                        let block_arrival_time = Duration::from_millis(
                            (block.proof.timeslot * TIMESLOT_DURATION) as u64,
                        );

                        let earliest_arrival_time = block_arrival_time - EPOCH_GRACE_PERIOD;
                        let latest_arrival_time = block_arrival_time + EPOCH_GRACE_PERIOD;

                        if genesis_instant.elapsed() < earliest_arrival_time {
                            error!(
                                "genesis instant {}, earliest arrival time {}",
                                genesis_instant.elapsed().as_millis(),
                                earliest_arrival_time.as_millis()
                            );

                            let wait_time = earliest_arrival_time - genesis_instant.elapsed();
                            error!("Received an early block via gossip, waiting {} ms for block arrival!", wait_time.as_millis());

                            let sender = main_to_main_tx.clone();
                            async_std::task::spawn(async move {
                                async_std::task::sleep(
                                    earliest_arrival_time
                                        .checked_sub(genesis_instant.elapsed())
                                        .unwrap_or_default(),
                                )
                                .await;

                                sender
                                    .send(ProtocolMessage::BlockArrived {
                                        block,
                                        peer_addr,
                                        cached: false,
                                    })
                                    .await;
                            })
                            .await;

                            continue;
                        }

                        if block_arrival_time > latest_arrival_time {
                            // block is too late, ignore
                            error!("Received a late block via gossip, ignoring");
                            continue;
                        }

                        // check that we have the randomness for the desired epoch
                        // then apply the block

                        let randomness_epoch =
                            epoch_tracker.get_lookback_epoch(block.proof.epoch).await;

                        if !randomness_epoch.is_closed {
                            panic!("Unable to apply block received via gossip, the randomness epoch is still open!");
                        }

                        // TODO: important -- this may lead to forks if nodes are malicious

                        // check if the block is valid and apply
                        if ledger.validate_and_apply_remote_block(block.clone()).await {
                            network
                                .regossip(&peer_addr, GossipMessage::BlockProposal { block })
                                .await;
                        }
                    }
                    GossipMessage::TxProposal { tx } => {
                        let tx_id = tx.get_id();
                        let mut ledger = ledger.lock().await;

                        // check to see if we already have the tx
                        if ledger.txs.contains_key(&tx_id) {
                            warn!("Received a duplicate tx via gossip, ignoring");
                            continue;
                        }

                        // validate the tx
                        let from_account_state = ledger.balances.get(&tx.from_address);
                        if !tx.is_valid(from_account_state) {
                            warn!("Received an invalid tx via gossip, ignoring");
                            continue;
                        }

                        // add to tx database and mempool
                        ledger.txs.insert(tx_id, Transaction::Credit(tx.clone()));
                        ledger.tx_mempool.insert(tx_id);

                        // re-gossip transaction
                        network
                            .regossip(&peer_addr, GossipMessage::TxProposal { tx })
                            .await;
                    }
                }
            }
        });
    }

    {
        let network = network.clone();
        let ledger = Arc::clone(&ledger);

        async_std::task::spawn(async move {
            let requests_receiver = network.get_requests_receiver().unwrap();
            while let Ok((message, response_sender)) = requests_receiver.recv().await {
                let ledger = Arc::clone(&ledger);

                async_std::task::spawn(async move {
                    match message {
                        RequestMessage::Blocks(BlocksRequest { block_height }) => {
                            // TODO: check to make sure that the requested timeslot is not ahead of local timeslot
                            let blocks = ledger.lock().await.get_blocks_by_height(block_height);

                            drop(
                                response_sender
                                    .send(ResponseMessage::Blocks(BlocksResponse { blocks })),
                            );
                        }
                    }
                });
            }
        });
    }

    let protocol_listener = async {
        info!("Main protocol loop is running...");

        // if gateway init the genesis block set and then start the timer
        if node_type == NodeType::Gateway {
            // init ledger from genesis
            let genesis_timestamp = ledger.lock().await.init_from_genesis().await;

            ledger.lock().await.timer_is_running = true;

            // start the timer
            start_timer(
                timer_to_farmer_tx.clone(),
                true,
                epoch_tracker.clone(),
                genesis_timestamp,
                CHALLENGE_LOOKBACK_EPOCHS * TIMESLOTS_PER_EPOCH,
                Arc::clone(&ledger),
            );
        }

        loop {
            match any_to_main_rx.recv().await {
                Ok(message) => {
                    match message {
                        ProtocolMessage::BlockArrived {
                            block,
                            peer_addr,
                            cached: _,
                        } => {
                            let mut ledger = ledger.lock().await;
                            info!(
                                "A new block has arrived with id: {}",
                                hex::encode(&block.get_id()[0..8])
                            );

                            if ledger.validate_and_apply_remote_block(block.clone()).await {
                                network
                                    .regossip(&peer_addr, GossipMessage::BlockProposal { block })
                                    .await;
                            }

                            // ToDo: Have to wipe cached blocks at some point to prevent memory leak

                            // if cached {
                            //     // block was cached and has arrived on sync
                            //     // check for more cached pending children
                            //     if let Some(children) =
                            //         ledger.pending_children_for_parent.get(&block_id)
                            //     {
                            //         arrive_pending_children(ledger, children.clone(), &main_to_main_tx)
                            //             .await;
                            //     }
                            // }
                        }
                        ProtocolMessage::BlockSolutions { solutions } => {
                            if !solutions.is_empty() {
                                for solution in solutions.into_iter() {
                                    let block = ledger
                                        .lock()
                                        .await
                                        .create_and_apply_local_block(solution)
                                        .await;
                                    network.gossip(GossipMessage::BlockProposal { block }).await;
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    error!("Error in protocol messages handling: {}", error);
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
                info!("New peer starting ledger sync with gateway");

                let is_farming = matches!(node_type, NodeType::Gateway | NodeType::Farmer);

                let mut timeslot: u64 = 0;
                let mut block_height = 0;
                loop {
                    match network.request_blocks(block_height).await {
                        Ok(blocks) => {
                            // TODO: may be able to work with timeslot since we are not creating blocks
                            let mut locked_ledger = ledger.lock().await;
                            // TODO: this is mainly for testing, later this will be replaced by state chain sync
                            // so there is no need for validating the block or timestamp

                            // first retrieves all applied blocks (ledger.applied_blocks_by_height)
                            // then retrieves all pending blocks (ledger.pending_blocks_by_height)
                            // finally retrieves all staged blocks (ledger.staged_blocks)
                            // if no blocks we whould be at (or near) the current timeslot
                            // will continue returning no blocks until we reach the current timeslot

                            // TODO: keep track of pending blocks in the console to see how long they remain pending

                            if blocks.len() > 0 {
                                let block_timeslot = blocks[0].proof.timeslot;

                                // skip forward to the timeslot for the next block height
                                while timeslot < block_timeslot {
                                    // advance epochs
                                    if (timeslot + 1) % TIMESLOTS_PER_EPOCH as u64 == 0 {
                                        // create new epoch
                                        let current_epoch = epoch_tracker
                                            .advance_epoch(&locked_ledger.blocks_on_longest_chain)
                                            .await;

                                        debug!(
                                            "Closed randomness for epoch {} during sync",
                                            current_epoch - 1
                                        );

                                        debug!(
                                            "Created a new empty epoch during sync blocks for index {}",
                                            current_epoch
                                        );
                                    }
                                    // advance timeslot
                                    timeslot += 1;
                                }

                                // stage each block for the block_height
                                for block in blocks.into_iter() {
                                    locked_ledger.stage_block(&block).await;
                                }

                                // apply all referenced blocks
                                locked_ledger.apply_referenced_blocks().await;
                            }

                            let next_timeslot_arrival_time = Duration::from_millis(
                                ((timeslot + 1) * TIMESLOT_DURATION)
                                    + locked_ledger.genesis_timestamp as u64,
                            );

                            let time_now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .expect("Time went backwards");

                            // check if we have arrived at the next timeslot
                            if next_timeslot_arrival_time < time_now {
                                // increment the epoch on boundary
                                if (timeslot + 1) % TIMESLOTS_PER_EPOCH as u64 == 0 {
                                    // create new epoch
                                    let current_epoch = epoch_tracker
                                        .advance_epoch(&locked_ledger.blocks_on_longest_chain)
                                        .await;

                                    debug!(
                                        "Closed randomness for epoch {} during sync",
                                        current_epoch - 1
                                    );

                                    debug!(
                                        "Created a new empty epoch during sync blocks for index {}",
                                        current_epoch
                                    );
                                }
                                // increment the timeslot
                                timeslot += 1;

                                // request the next block height
                                block_height += 1;
                            } else {
                                // once we have all blocks, apply cached gossip

                                // call sync and start timer
                                info!("Applying cached blocks");
                                match locked_ledger.apply_cached_blocks(block_height).await {
                                    Ok(timeslot) => {
                                        info!("Starting the timer from genesis time");

                                        locked_ledger.timer_is_running = true;

                                        // start the timer
                                        start_timer(
                                            timer_to_farmer_tx.clone(),
                                            is_farming,
                                            epoch_tracker.clone(),
                                            locked_ledger.genesis_timestamp,
                                            timeslot,
                                            Arc::clone(&ledger),
                                        );
                                    }
                                    Err(_) => {
                                        panic!("Unable to sync the ledger, invalid blocks!");
                                    }
                                }
                                break;
                            }
                        }
                        Err(error) => {
                            // TODO: Not panic, retry
                            panic!(
                                "Failed to request blocks for block_height {}: {:?}",
                                block_height, error
                            );
                        }
                    }
                }

                // on genesis block

                // init the timer
            }
        };

        // send state update requests in a loop to network
        if CONSOLE {
            loop {
                let mut state = network.get_state().await;
                state.node_type = node_type.to_string();
                state.peers = state.peers + "/" + &MIN_PEERS.to_string()[..];
                state.blocks = "TODO".to_string();
                state.pieces = match node_type {
                    NodeType::Gateway => PLOT_SIZE.to_string(),
                    NodeType::Farmer => PLOT_SIZE.to_string(),
                    NodeType::Peer => 0.to_string(),
                };
                state_sender.send(state).unwrap();

                task::sleep(Duration::from_millis(1000)).await;
            }
        }
    };

    join!(protocol_listener, protocol_startup);
}
