use async_std::sync::channel;
use async_std::task;
use console::AppState;
use crossbeam_channel::unbounded;
use futures::join;
use log::LevelFilter;
use log::*;
use std::path::PathBuf;
use std::thread;
use std::{env, fs};
use subspace_core_rust::farmer::FarmerMessage;
use subspace_core_rust::ledger::Ledger;
use subspace_core_rust::manager::ProtocolMessage;
use subspace_core_rust::network::{Network, NodeType};
use subspace_core_rust::plot::Plot;
use subspace_core_rust::pseudo_wallet::Wallet;
use subspace_core_rust::timer::EpochTracker;
use subspace_core_rust::{
    console, farmer, manager, plotter, rpc, state, CONSOLE, DEV_GATEWAY_ADDR,
    MAINTAIN_PEERS_INTERVAL, MAX_CONNECTED_PEERS, MAX_PEERS, MIN_CONNECTED_PEERS, MIN_PEERS,
};
use tui_logger::{init_logger, set_default_level};

/* TODO

 Next Steps

   - complete state encoding workflow
   - improve console UX
   - finish p2p network impl
   - add remaining RPC messages

   - Compute and enforce cost of storage
   - Storage accounts
   - Switch to Schnorr signatures
   - Improve tx script support
   - Add nonce to tag computation

 Fixes

   - recover from missed gossip (else diverges) -> request blocks by parent_id
   - complete fine tuning parameters
   - initial tx validation is too strict (requires k-deep confirmation)
   - recover from deep forks at network partitions
   - derive randomness from the block with the most confirmations (not earliest)

 Security

   - Ensure that an attacker cannot crash a node by intentionally creating a panic condition
   - No way to malleate on the difficulty target


*/

#[async_std::main]
async fn main() {
    /*
     * Startup: cargo run <node_type> <custom_path>
     *
     * arg1 type -> gateway, farmer, peer (gateway default)
     * arg2 path -> unique path for plot (data_local_dir default)
     *
     * Later: plot size, env
     *
     */

    // create an async channel for console
    let (app_state_sender, app_state_receiver) = unbounded::<AppState>();

    if CONSOLE {
        // setup the logger
        init_logger(LevelFilter::Debug).unwrap();
        set_default_level(LevelFilter::Trace);

        // spawn a new thread to run the node else it will block the console
        thread::spawn(move || {
            task::spawn(async move {
                run(app_state_sender).await;
            });
        });

        // run the console app in the foreground, passing the receiver
        console::run(app_state_receiver).unwrap();
    } else {
        // TODO: fix default log level and occasionally print state to the console
        env_logger::init();
        run(app_state_sender).await;
    }
}

pub async fn run(app_state_sender: crossbeam_channel::Sender<AppState>) {
    let node_addr = "127.0.0.1:0".parse().unwrap();
    let node_type = env::args()
        .skip(1)
        .take(1)
        .next()
        .map(|s| s.parse().ok())
        .flatten()
        .unwrap_or(NodeType::Gateway);

    // set storage path
    let path = env::args()
        .nth(2)
        .or_else(|| std::env::var("SUBSPACE_DIR").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .expect("Can't find local data directory, needs to be specified explicitly")
                .join("subspace")
                .join("results")
        });

    if !path.exists() {
        fs::create_dir_all(&path).unwrap_or_else(|error| {
            panic!("Failed to create data directory {:?}: {:?}", path, error)
        });
    }

    info!(
        "Starting new Subspace {:?} using location {:?}",
        node_type, path
    );

    let wallet = Wallet::open_or_create(&path).expect("Failed to init wallet");
    let keys = wallet.keypair;
    let node_id = wallet.node_id;

    // create the randomness tracker
    let epoch_tracker = if node_type == NodeType::Gateway {
        EpochTracker::new_genesis().await
    } else {
        EpochTracker::new()
    };

    let is_farming = matches!(node_type, NodeType::Gateway | NodeType::Farmer);

    // create channels between background tasks
    let (any_to_main_tx, any_to_main_rx) = channel::<ProtocolMessage>(32);
    let (timer_to_farmer_tx, timer_to_farmer_rx) = channel::<FarmerMessage>(32);
    let solver_to_main_tx = any_to_main_tx.clone();
    let state_to_plotter_tx = any_to_main_tx.clone();

    // create the state
    let mut state = state::State::new(state_to_plotter_tx);

    // optionally create the plot
    let plot_option: Option<Plot> = match node_type {
        NodeType::Gateway => {
            // create the genesis state
            let state_bundle = state.create_genesis_state("SUBSPACE", 256).await.unwrap();

            // create the plot from genesis state
            Some(plotter::plot(path.into(), node_id, state_bundle.piece_bundles).await)
        }
        NodeType::Farmer => {
            // TODO: create an empty plot and send to manager
            None
        }
        _ => None,
    };

    // create the ledger
    let ledger = Ledger::new(keys, epoch_tracker.clone(), state);

    // create the network
    let network_fut = Network::new(
        node_id,
        if node_type == NodeType::Gateway {
            DEV_GATEWAY_ADDR.parse().unwrap()
        } else {
            node_addr
        },
        MIN_CONNECTED_PEERS,
        MAX_CONNECTED_PEERS,
        MIN_PEERS,
        MAX_PEERS,
        MAINTAIN_PEERS_INTERVAL,
    );
    let network = network_fut.await.unwrap();

    // initiate outbound netowrk connections
    if node_type != NodeType::Gateway {
        info!("Connecting to gateway node");

        network
            .connect_to(DEV_GATEWAY_ADDR.parse().unwrap())
            .await
            .unwrap();

        // Connect to more peers if possible
        for _ in 0..MIN_CONNECTED_PEERS {
            if let Some(peer) = network.get_random_disconnected_peer().await {
                drop(network.connect_to(peer).await);
            }
        }
    }

    // optionally create the RPC server
    let mut rpc_server = None;
    if std::env::var("RUN_WS_RPC")
        .map(|value| value == "1".to_string())
        .unwrap_or_default()
    {
        rpc_server = Some(rpc::run(node_id, network.clone()));
    }

    // create the manager
    let manager = manager::run(
        node_type,
        ledger,
        any_to_main_rx,
        network,
        app_state_sender,
        timer_to_farmer_tx,
        epoch_tracker,
        plot_option.clone(),
    );

    if is_farming {
        let plot = plot_option.unwrap();
        let farmer = farmer::run(timer_to_farmer_rx, solver_to_main_tx, &plot);
        join!(manager, farmer);
    } else {
        join!(manager);
    }

    // RPC server will stop when this is dropped
    drop(rpc_server);
}
