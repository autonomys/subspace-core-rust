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
    timer, ContentId, CHALLENGE_LOOKBACK_EPOCHS, CONSOLE, MIN_PEERS, PLOT_SIZE,
    TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION,
};
use async_std::sync::{Receiver, Sender};
use async_std::task;
use futures::join;
use futures::lock::Mutex;
use log::*;
use std::fmt;
use std::fmt::Display;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

// TODO: start the timer once
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
    state_sender: crossbeam_channel::Sender<AppState>,
    timer_to_farmer_tx: Sender<FarmerMessage>,
    epoch_tracker: EpochTracker,
) {
    let ledger: SharedLedger = Arc::new(Mutex::new(ledger));
    {
        let network = network.clone();
        let ledger = Arc::clone(&ledger);

        async_std::task::spawn(async move {
            let gossip_receiver = network.get_gossip_receiver().unwrap();
            while let Ok((peer_addr, message)) = gossip_receiver.recv().await {
                match message {
                    GossipMessage::BlockProposal { block } => {
                        info!("Received a new block via gossip");
                        let mut locked_ledger = ledger.lock().await;

                        if locked_ledger.validate_block_from_gossip(&block).await {
                            network
                                .regossip(
                                    &peer_addr,
                                    GossipMessage::BlockProposal {
                                        block: block.clone(),
                                    },
                                )
                                .await;

                            if block.content.parent_id.is_some() {
                                // stage the proposer block
                                locked_ledger.stage_proposer_block(&block).await;

                                // stage any cached children
                                locked_ledger
                                    .stage_cached_children(block.content.get_id())
                                    .await;
                            } else {
                                // stage the tx block
                                locked_ledger.stage_tx_block(&block).await;
                            }
                        }
                    }
                    GossipMessage::TxProposal { tx } => {
                        let tx_id = tx.get_id();
                        let mut locked_ledger = ledger.lock().await;

                        // check to see if we already have the tx
                        if locked_ledger.txs.contains_key(&tx_id) {
                            warn!("Received a duplicate tx via gossip, ignoring");
                            continue;
                        }

                        // only validate and gossip if synced
                        if locked_ledger.timer_is_running {
                            // validate the tx
                            let from_account_state = locked_ledger.balances.get(&tx.from_address);
                            if !tx.is_valid(from_account_state) {
                                warn!("Received an invalid tx via gossip, ignoring");
                                continue;
                            }

                            // re-gossip transaction
                            network
                                .regossip(&peer_addr, GossipMessage::TxProposal { tx: tx.clone() })
                                .await;
                        }

                        // add to tx database and mempool
                        locked_ledger.txs.insert(tx_id, Transaction::Credit(tx));

                        if locked_ledger.unknown_tx_ids.contains(&tx_id) {
                            locked_ledger.unknown_tx_ids.remove(&tx_id);
                        } else {
                            locked_ledger.unclaimed_tx_ids.insert(tx_id);
                        }
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
                            let locked_ledger = ledger.lock().await;
                            let blocks = locked_ledger.get_blocks_by_timeslot(timeslot);
                            let transactions = locked_ledger.get_txs_for_sync(&blocks);

                            drop(
                                response_sender.send(ResponseMessage::Blocks(BlocksResponse {
                                    blocks,
                                    transactions,
                                })),
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

                                if block.content.parent_id.is_some() {
                                    ledger.lock().await.stage_proposer_block(&block).await;
                                    content_ids.push(block.content.get_id());
                                } else {
                                    ledger.lock().await.stage_tx_block(&block).await;
                                }
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
                    info!("Requesting blocks for timeslot {} during sync", timeslot);
                    match network.request_blocks(timeslot).await {
                        Ok(bundle) => {
                            let mut locked_ledger = ledger.lock().await;

                            for block in bundle.0.iter() {
                                if timeslot == 0 {
                                    // start the timer from genesis time
                                    // TODO: start timer at node init once we have canonical genesis time
                                }

                                // TODO: must include the proof in block now (no pruning)

                                // validate the block
                                if !locked_ledger
                                    .validate_block_from_sync(&block, timeslot)
                                    .await
                                {
                                    // TODO: start over with a different peer
                                }

                                // stage the block
                                if block.content.parent_id.is_some() {
                                    locked_ledger.stage_proposer_block(&block).await;
                                } else {
                                    locked_ledger.stage_tx_block(&block).await;
                                }
                            }

                            for tx in bundle.1.iter() {
                                let tx_id = tx.get_id();
                                let mut locked_ledger = ledger.lock().await;

                                // check to see if we already have the tx
                                if locked_ledger.txs.contains_key(&tx_id) {
                                    warn!("Received a duplicate tx via gossip, ignoring");
                                    continue;
                                }

                                // TODO: should we validate the transaction?

                                // add to tx database and mempool
                                locked_ledger
                                    .txs
                                    .insert(tx_id, Transaction::Credit(tx.clone()));

                                if locked_ledger.unknown_tx_ids.contains(&tx_id) {
                                    locked_ledger.unknown_tx_ids.remove(&tx_id);
                                } else {
                                    locked_ledger.unclaimed_tx_ids.insert(tx_id);
                                }
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

                                locked_ledger.timer_is_running = true;
                                locked_ledger.current_timeslot = timeslot - 1;

                                // start the timer
                                info!("Starting the timer from genesis time");
                                start_timer(
                                    timer_to_farmer_tx.clone(),
                                    is_farming,
                                    epoch_tracker.clone(),
                                    locked_ledger.genesis_timestamp,
                                    timeslot,
                                    Arc::clone(&ledger),
                                );

                                // apply any proposer blocks from cached gossip
                                for block in bundle.0.iter() {
                                    locked_ledger
                                        .stage_cached_children(block.content.get_id())
                                        .await;
                                }

                                // apply any cached tx block from gossip
                                let cached_tx_blocks: Vec<(ContentId, Block)> = locked_ledger
                                    .cached_tx_blocks_by_content_id
                                    .drain()
                                    .collect();

                                for (content_id, tx_block) in cached_tx_blocks.iter() {
                                    if !locked_ledger.metablocks.contains_content_id(content_id) {
                                        if locked_ledger.validate_block_from_cache(tx_block).await {
                                            locked_ledger.stage_tx_block(tx_block).await;
                                        }
                                    }
                                }

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
