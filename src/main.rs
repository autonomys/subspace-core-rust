use async_std::sync::channel;
use async_std::task;
use console::AppState;
use crossbeam_channel::unbounded;
use futures;
use log::LevelFilter;
use log::*;
use std::path::PathBuf;
use std::thread;
use std::{env, fs};
use subspace_core_rust::farmer::FarmerMessage;
use subspace_core_rust::ledger::Ledger;
use subspace_core_rust::manager::ProtocolMessage;
use subspace_core_rust::network::{NodeType, StartupNetwork};
use subspace_core_rust::plot::Plot;
use subspace_core_rust::pseudo_wallet::Wallet;
use subspace_core_rust::timer::EpochTracker;
use subspace_core_rust::{
    console, farmer, manager, network, plotter, rpc, state, BLOCK_LIST_SIZE, CONSOLE,
    DEV_GATEWAY_ADDR, GENESIS_PIECE_COUNT, MAINTAIN_PEERS_INTERVAL, MAX_CONTACTS, MAX_PEERS,
    MIN_CONTACTS, MIN_PEERS,
};
use tui_logger::{init_logger, set_default_level};

/* TODO

 Next Steps

   - complete state encoding workflow
   - improve console UX
   - test transaction throughput
   - finish p2p network impl (Nazar)

   - Compute and enforce cost of storage
   - Storage accounts
   - Switch to Schnorr signatures
   - Improve tx script support
   - Add nonce to tag computation

 Fixes

   - request block by content_id once if unknown parent on missed gossip
   - complete fine tuning parameters
   - initial tx validation is too strict (requires k-deep confirmation)
   - recover from deep forks at network partitions

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

    // create or open wallet
    let wallet = Wallet::open_or_create(&path).expect("Failed to init wallet");
    let keys = wallet.keypair;
    let node_id = wallet.node_id;

    // create the randomness tracker
    let epoch_tracker = EpochTracker::new().await;

    // create channels between background tasks
    let (any_to_main_tx, any_to_main_rx) = channel::<ProtocolMessage>(128);
    let (timer_to_farmer_tx, timer_to_farmer_rx) = channel::<FarmerMessage>(32);
    let solver_to_main_tx = any_to_main_tx.clone();
    let state_to_plotter_tx = any_to_main_tx.clone();

    // create the state
    let mut state = state::State::new(state_to_plotter_tx);

    // create the plot
    let plot: Plot = match node_type {
        NodeType::Gateway => {
            // create the genesis state
            let state_bundle = state
                .create_genesis_state("SUBSPACE", GENESIS_PIECE_COUNT)
                .await;

            // create the plot from genesis state
            plotter::plot(path.into(), node_id, state_bundle).await
        }
        _ => {
            // create an empty plot
            plotter::plot(path.into(), node_id, vec![]).await
        }
    };

    // create the farming loop
    let farmer = farmer::run(timer_to_farmer_rx, solver_to_main_tx, &plot);

    // create the ledger
    let ledger = Ledger::new(keys, epoch_tracker.clone(), state);

    // create the network
    let startup_network_fut = StartupNetwork::new(
        node_id,
        if node_type == NodeType::Gateway {
            DEV_GATEWAY_ADDR.parse().unwrap()
        } else {
            node_addr
        },
        MIN_PEERS,
        MAX_PEERS,
        MIN_CONTACTS,
        MAX_CONTACTS,
        BLOCK_LIST_SIZE,
        MAINTAIN_PEERS_INTERVAL,
        network::create_backoff,
    );

    // initiate outbound network connections
    let startup_network = startup_network_fut.await.unwrap();
    if node_type != NodeType::Gateway {
        info!("Connecting to gateway node");

        let contacts_level = startup_network
            .connect(DEV_GATEWAY_ADDR.parse().unwrap())
            .await
            .expect("Failed to connect to a single gateway node");

        // TODO: Min gateways check

        if !contacts_level.min_contacts() {
            panic!("Failed to reach min contacts level on startup");
        }

        loop {
            // TODO: Failed attempts should be handled correctly
            match startup_network.connect_to_random_contact().await {
                Ok(peers_level) => {
                    if peers_level.min_peers() {
                        break;
                    }
                }
                Err(error) => {
                    error!(
                        "Failed to connect to minimum number of peers on startup: {:?}",
                        error
                    );
                    break;
                }
            }
        }
    }
    let network = startup_network.finish_startup();

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
        node_id,
        ledger,
        any_to_main_rx,
        network,
        app_state_sender,
        timer_to_farmer_tx,
        epoch_tracker,
        plot.clone(),
    );

    futures::join!(manager, farmer);

    // RPC server will stop when this is dropped
    drop(rpc_server);
}
