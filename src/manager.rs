use crate::block::Block;
use crate::console::AppState;
use crate::farmer::{FarmerMessage, Solution};
use crate::ledger::Ledger;
use crate::network::messages::{
    BlockRequestByContentId, BlockRequestByProofId, BlockResponseByContentId,
    BlockResponseByProofId, BlocksRequest, BlocksResponse, GenesisConfigRequest,
    GenesisConfigResponse, GossipMessage, PieceRequestById, PieceRequestByIndex,
    PieceResponseByIndex, RequestMessage, ResponseMessage, StateBlockRequestByHeight,
    StateBlockRequestById, StateBlockResponseByHeight, StateBlockResponseById, TxRequestById,
    TxResponseById,
};
use crate::network::{Network, NodeType};
use crate::plot::Plot;
use crate::state::StateBundle;
use crate::timer::EpochTracker;
use crate::transaction::Transaction;
use crate::{
    crypto, sloth, timer, ContentId, NodeID, CONSOLE, ENCODING_LAYERS_TEST, GENESIS_STATE_BLOCKS,
    MIN_PEERS, PIECES_PER_STATE_BLOCK, PLOT_SIZE, PRIME_SIZE_BITS, TIMESLOTS_PER_EPOCH,
    TIMESLOT_DURATION,
};
use async_std::sync::{Receiver, Sender};
use async_std::task;
use futures::lock::Mutex;
use log::*;
use rug::integer::Order;
use rug::Integer;
use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use std::fmt;
use std::fmt::Display;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct GenesisConfig {
    pub genesis_timestamp: u64,
    pub genesis_challenge: [u8; 32],
}

pub enum ProtocolMessage {
    /// Solver sends a set of solutions back to main for application
    BlockSolutions {
        solutions: Vec<Solution>,
    },
    StateBundle {
        state_bundle: StateBundle,
    },
}

impl Display for ProtocolMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::BlockSolutions { .. } => "BlockSolutions",
                Self::StateBundle { .. } => "StateBundle",
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
    node_id: NodeID,
    ledger: Ledger,
    any_to_main_rx: Receiver<ProtocolMessage>,
    network: Network,
    state_sender: crossbeam_channel::Sender<AppState>,
    timer_to_farmer_tx: Sender<FarmerMessage>,
    epoch_tracker: EpochTracker,
    plot: Plot,
) {
    let ledger: SharedLedger = Arc::new(Mutex::new(ledger));

    let gossip_handling = {
        let network = network.clone();
        let ledger = Arc::clone(&ledger);

        async move {
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
                            // TODO: validate without account state
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
        }
    };

    let requests_handling = {
        let network = network.clone();
        let ledger = Arc::clone(&ledger);
        // let plot = Arc::clone(&plot);
        let plot = plot.clone();

        async move {
            let requests_receiver = network.get_requests_receiver().unwrap();
            while let Ok((message, response_sender)) = requests_receiver.recv().await {
                let ledger = Arc::clone(&ledger);
                // let plot = Arc::clone(&plot);
                let plot = plot.clone();

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
                        RequestMessage::BlockByContentId(BlockRequestByContentId { id }) => {
                            let locked_ledger = ledger.lock().await;
                            let block: Option<Block> = match locked_ledger
                                .metablocks
                                .get_metablock_from_content_id_as_option(&id)
                            {
                                Some(metablock) => Some(metablock.block),
                                None => None,
                            };

                            drop(response_sender.send(ResponseMessage::BlockByContentId(
                                BlockResponseByContentId { block },
                            )));
                        }
                        RequestMessage::BlockByProofId(BlockRequestByProofId { id }) => {
                            let locked_ledger = ledger.lock().await;
                            let block: Option<Block> = match locked_ledger
                                .metablocks
                                .get_metablock_from_proof_id_as_option(&id)
                            {
                                Some(metablock) => Some(metablock.block),
                                None => None,
                            };

                            drop(response_sender.send(ResponseMessage::BlockByProofId(
                                BlockResponseByProofId { block },
                            )));
                        }
                        RequestMessage::TransactionById(TxRequestById { id }) => {
                            let locked_ledger = ledger.lock().await;
                            let transaction: Option<Transaction> = match locked_ledger.txs.get(&id)
                            {
                                Some(tx) => Some(tx.clone()),
                                None => None,
                            };

                            drop(response_sender.send(ResponseMessage::TransactionById(
                                TxResponseById { transaction },
                            )));
                        }
                        RequestMessage::StateById(StateBlockRequestById { id }) => {
                            let locked_ledger = ledger.lock().await;
                            let state_block = match locked_ledger.state.get_state_block_by_id(&id) {
                                Some(block) => Some(block.clone()),
                                None => None,
                            };

                            drop(response_sender.send(ResponseMessage::StateById(
                                StateBlockResponseById { state_block },
                            )));
                        }
                        RequestMessage::StateByHeight(StateBlockRequestByHeight { height }) => {
                            let locked_ledger = ledger.lock().await;
                            let state_block =
                                match locked_ledger.state.get_state_block_by_height(height) {
                                    Some(block) => Some(block.clone()),
                                    None => None,
                                };

                            drop(response_sender.send(ResponseMessage::StateByHeight(
                                StateBlockResponseByHeight { state_block },
                            )));
                        }
                        RequestMessage::PieceByIndex(PieceRequestByIndex { index }) => {
                            let piece_bundle = plot.get_piece_bundle_by_index(index, node_id).await;

                            drop(response_sender.send(ResponseMessage::PieceByIndex(
                                PieceResponseByIndex { piece_bundle },
                            )));
                        }
                        RequestMessage::PieceById(PieceRequestById { id: _ }) => {
                            // TODO: store pieces by piece_id for retrieval
                        }
                        RequestMessage::GenesisConfig(GenesisConfigRequest {}) => {
                            let locked_ledger = ledger.lock().await;
                            let genesis_config = GenesisConfig {
                                genesis_timestamp: locked_ledger.genesis_timestamp,
                                genesis_challenge: locked_ledger.genesis_challenge,
                            };

                            drop(response_sender.send(ResponseMessage::GenesisConfig(
                                GenesisConfigResponse { genesis_config },
                            )));
                        }
                    }
                });
            }
        }
    };

    let protocol_listener = async {
        info!("Main protocol loop is running...");

        // if gateway init the genesis block set and then start the timer
        if node_type == NodeType::Gateway {
            // set genesis time
            let genesis_timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_millis() as u64;

            // start the timer
            start_timer(
                timer_to_farmer_tx.clone(),
                true,
                epoch_tracker.clone(),
                genesis_timestamp,
                1,
                Arc::clone(&ledger),
            );

            {
                let mut locked_ledger = ledger.lock().await;
                locked_ledger.timer_is_running = true;
                locked_ledger.genesis_timestamp = genesis_timestamp;
                locked_ledger.current_timeslot = 0;
            }

            // how to send the genesis timestamp to new nodes? ledger.stage_proposer_block
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
                    ProtocolMessage::StateBundle { state_bundle } => {
                        // TODO: move encoding of new state to a background task
                        // then write to plot in batches

                        if state_bundle.state_block.height >= GENESIS_STATE_BLOCKS as u64 {
                            // TODO: only plot pieces that evict other pieces
                            warn!("Plotting pieces from new confirmed state");
                            plot.plot_pieces(node_id, state_bundle.piece_bundles).await;
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
                info!("Starting new gateway node from genesis");
            }
            NodeType::Peer | NodeType::Farmer => {
                info!("New peer starting ledger sync with genesis gateway");

                let is_farming = matches!(node_type, NodeType::Gateway | NodeType::Farmer);

                /*
                   New Startup workflow

                   Initial Case

                   - get genesis config
                   - sync the state chain
                   - sync state and plot
                   - sync head of the ledger and start solving

                   * if ledger state has been encoded before sync, it will be re-encoded on ledger sync

                   Ideal Case

                   1. Query several nodes for the last state block and see if they agree
                   2. Sync the state chain, starting from height 0 until a None is returned
                   3. Then sync head of the ledger
                   4. Then conduct logarithmic verification of state blocks
                   5. Once the state chain and head of ledger have been validated
                   6. Fetch pieces over the DHT from closest farmers
                   7. Decode and verify each piece against the state chain
                   8. Plot each piece locally
                   9. Start solving once the plot is full

                   Need to define a workflow for piece retrieval and plotting

                   1. Send a request for a range of pieces
                   2. Receive as a stream
                   3. For each piece received, decode and validate against state chain
                   4. Write the piece to disk
                   5. Add the piece to a queue
                   6. Read, encode, and plot the piece once it arrives in the queue
                */

                // sync the genesis config
                match network.request_genesis_config().await {
                    Ok(genesis_config) => {
                        ledger.lock().await.set_genesis_config(genesis_config);
                    }
                    Err(error) => {
                        panic!("Failed to request genesis config: {:?}", error);
                    }
                }

                // sync the state chain, verify each new block and add to the state chain
                let mut state_block_height = 0;
                let mut last_state_block_id = [0u8; 32];
                let mut locked_ledger = ledger.lock().await;
                loop {
                    match network
                        .request_state_block_by_height(state_block_height)
                        .await
                    {
                        Ok(state_block) => {
                            match state_block {
                                Some(state_block) => {
                                    if state_block_height > 0 {
                                        if state_block.previous_state_block_id
                                            != last_state_block_id
                                        {
                                            // TODO: should block list this peer and request from another peer
                                            panic!("Received an invalid state block during state chain sync");
                                        }
                                    }

                                    last_state_block_id = state_block.get_id();
                                    state_block_height += 1;
                                    locked_ledger.state.save_state_block(&state_block);
                                }
                                None => break,
                            }
                        }
                        Err(error) => {
                            panic!(
                                "Failed to request state block for height {}: {:?}",
                                state_block_height, error
                            );
                        }
                    }
                }
                info!("Synced the state chain!");

                // TODO: Handle the edge case where ...
                // TODO: what if the we sync the state chain, then a new state block is encoded before and we try to sync those pieces with no merkle root to validate against

                // sync state and plot
                let mut piece_index = 0;
                let sloth = sloth::Sloth::init(PRIME_SIZE_BITS);
                loop {
                    match network.request_piece_by_index(piece_index).await {
                        Ok(piece_bundle) => {
                            match piece_bundle {
                                Some(piece_bundle) => {
                                    // decode the piece
                                    let expanded_iv = crypto::expand_iv(piece_bundle.node_id);
                                    let mut decoding = piece_bundle.encoding.clone();
                                    sloth.decode(
                                        decoding.as_mut(),
                                        expanded_iv,
                                        ENCODING_LAYERS_TEST,
                                    );

                                    // compute the piece hash
                                    let decoding_hash = crypto::digest_sha_256(&decoding);

                                    // use the piece index to compute the state block and get merkle root
                                    let state_block_index =
                                        piece_index / PIECES_PER_STATE_BLOCK as u64;

                                    let merkle_root = locked_ledger
                                        .state
                                        .get_state_block_by_height(state_block_index)
                                        .unwrap()
                                        .piece_merkle_root;

                                    // verify the merkle proof
                                    if !crypto::validate_merkle_proof(
                                        decoding_hash,
                                        &piece_bundle.piece_proof,
                                        &merkle_root,
                                    ) {
                                        panic!("Invalid piece received via sync, merkle proof is invalid!");
                                    }

                                    // encode with node_id
                                    let expanded_iv = crypto::expand_iv(node_id);
                                    let integer_expanded_iv =
                                        Integer::from_digits(&expanded_iv, Order::Lsf);
                                    let mut encoding = decoding[..].try_into().unwrap();

                                    sloth
                                        .encode(
                                            &mut encoding,
                                            &integer_expanded_iv,
                                            ENCODING_LAYERS_TEST,
                                        )
                                        .unwrap();

                                    // compute the nonce
                                    let nonce = u64::from_le_bytes(
                                        crypto::create_hmac(&encoding, b"subspace")[0..8]
                                            .try_into()
                                            .unwrap(),
                                    );

                                    // add to the plot
                                    let result = plot
                                        .write(
                                            encoding,
                                            nonce,
                                            piece_index,
                                            piece_bundle.piece_proof,
                                        )
                                        .await;

                                    if let Err(error) = result {
                                        panic!("{}", error);
                                    }

                                    piece_index += 1;
                                }
                                None => break,
                            }
                        }
                        Err(error) => {
                            panic!(
                                "Failed to request piece for index {}: {:?}",
                                piece_index, error
                            );
                        }
                    }
                }
                info!("Synced state and completed plotting!");

                // TODO: just sync head of the ledger
                let mut timeslot: u64 = 0;
                loop {
                    info!("Requesting blocks for timeslot {} during sync", timeslot);
                    match network.request_blocks(timeslot).await {
                        Ok(bundle) => {
                            // let mut locked_ledger = ledger.lock().await;

                            for tx in bundle.1.iter() {
                                let tx_id = tx.get_id();

                                // check to see if we already have the tx
                                if locked_ledger.txs.contains_key(&tx_id) {
                                    warn!("Received a duplicate tx via gossip, ignoring");
                                    continue;
                                }

                                // TODO: tx should be validated internally only

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

                            for block in bundle.0.iter() {
                                if timeslot == 0 {
                                    // start the timer from genesis time
                                    // TODO: start timer at node init once we have canonical genesis time
                                }

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

    futures::join!(
        gossip_handling,
        requests_handling,
        protocol_listener,
        protocol_startup
    );
}
