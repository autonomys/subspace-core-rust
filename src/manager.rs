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
    timer, ContentId, CHALLENGE_LOOKBACK_EPOCHS, CONSOLE, EPOCH_GRACE_PERIOD, MAX_EARLY_TIMESLOTS,
    MAX_LATE_TIMESLOTS, MIN_CONNECTED_PEERS, PLOT_SIZE, TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION,
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
}

impl Display for ProtocolMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::BlockSolutions { .. } => "BlockSolutions",
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
                        info!("Received a new block via gossip");
                        let mut locked_ledger = ledger.lock().await;

                        if locked_ledger
                            .is_valid_proposer_block_from_gossip(&block)
                            .await
                        {
                            network
                                .regossip(
                                    &peer_addr,
                                    GossipMessage::BlockProposal {
                                        block: block.clone(),
                                    },
                                )
                                .await;

                            // stage the block
                            locked_ledger.stage_block(&block).await;

                            // stage any cached children
                            locked_ledger
                                .stage_cached_children(block.content.get_id())
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
                        RequestMessage::Blocks(BlocksRequest { timeslot }) => {
                            // TODO: check to make sure that the requested timeslot is not ahead of local timeslot
                            let blocks = ledger.lock().await.get_blocks_by_timeslot(timeslot);

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

            let next_timeslot = CHALLENGE_LOOKBACK_EPOCHS * TIMESLOTS_PER_EPOCH;

            {
                let mut locked_ledger = ledger.lock().await;
                locked_ledger.timer_is_running = true;
                locked_ledger.current_timeslot = next_timeslot - 1;
            }

            // start the timer
            start_timer(
                timer_to_farmer_tx.clone(),
                true,
                epoch_tracker.clone(),
                genesis_timestamp,
                next_timeslot,
                Arc::clone(&ledger),
            );
        }

        loop {
            match any_to_main_rx.recv().await {
                Ok(message) => match message {
                    ProtocolMessage::BlockSolutions { solutions } => {
                        if !solutions.is_empty() {
                            let mut content_ids: Vec<ContentId> = Vec::new();
                            for solution in solutions.into_iter() {
                                let block = ledger
                                    .lock()
                                    .await
                                    .create_and_apply_local_block(solution, content_ids.clone())
                                    .await;
                                network
                                    .gossip(GossipMessage::BlockProposal {
                                        block: block.clone(),
                                    })
                                    .await;

                                info!("Gossiping a new valid block to all peers");

                                content_ids.push(block.content.get_id());
                                ledger.lock().await.stage_block(&block).await;
                            }
                        }
                    }
                },
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
                info!("New peer starting ledger sync with gateway");

                let is_farming = matches!(node_type, NodeType::Gateway | NodeType::Farmer);

                let mut timeslot: u64 = 0;
                loop {
                    match network.request_blocks(timeslot).await {
                        Ok(blocks) => {
                            let mut locked_ledger = ledger.lock().await;

                            for block in blocks.iter() {
                                if timeslot == 0 {
                                    // start the timer from genesis time
                                    // TODO: start timer at node init once we have canonical genesis time
                                }

                                // TODO: must include the proof in block now (no pruning)

                                // validate the block
                                if !locked_ledger
                                    .is_valid_proposer_block_from_sync(&block, timeslot)
                                    .await
                                {
                                    // TODO: start over with a different peer
                                }

                                // stage the block
                                locked_ledger.stage_block(&block).await;
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
                                    let current_epoch = epoch_tracker.advance_epoch().await;

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
                            } else {
                                info!("Reached the current timeslot during sync, applying any cached gossip and starting the timer");

                                // apply any cached gossip
                                for block in blocks.iter() {
                                    locked_ledger
                                        .stage_cached_children(block.content.get_id())
                                        .await;
                                }

                                info!("Starting the timer from genesis time");

                                locked_ledger.timer_is_running = true;
                                locked_ledger.current_timeslot = timeslot - 1;

                                // start the timer
                                start_timer(
                                    timer_to_farmer_tx.clone(),
                                    is_farming,
                                    epoch_tracker.clone(),
                                    locked_ledger.genesis_timestamp,
                                    timeslot,
                                    Arc::clone(&ledger),
                                );

                                break;
                            }
                        }
                        Err(error) => {
                            // TODO: Not panic, retry
                            panic!(
                                "Failed to request blocks for timeslot {}: {:?}",
                                timeslot, error
                            );
                        }
                    }
                }
            }
        };

        // send state update requests in a loop to network
        if CONSOLE {
            loop {
                let mut state = network.get_state().await;
                state.node_type = node_type.to_string();
                state.peers = state.peers + "/" + &MIN_CONNECTED_PEERS.to_string()[..];
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
