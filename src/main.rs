use async_std::sync::channel;
use async_std::task;
use console::AppState;
use crossbeam_channel::unbounded;
use futures::join;
use log::LevelFilter;
use log::*;
use manager::ProtocolMessage;
use network::{Network, NodeType};
use std::path::PathBuf;
use std::thread;
use std::{env, fs};
use subspace_core_rust::farmer::FarmerMessage;
use subspace_core_rust::ledger::Ledger;
use subspace_core_rust::pseudo_wallet::Wallet;
use subspace_core_rust::timer::EpochTracker;
use subspace_core_rust::*;
use tui_logger::{init_logger, set_default_level};

/* ToDo
 * ----
 *
 * Implementation Security
 * -----------------------
 * Ensure that block and tx signatures are not malleable
 * Ensure that an attacker cannot crash a node by intentionally creating a panic condition
 * No way to malleate on the difficulty target
 *
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
    let (state_sender, state_receiver) = unbounded::<AppState>();

    if CONSOLE {
        // setup the logger
        init_logger(LevelFilter::Debug).unwrap();
        set_default_level(LevelFilter::Trace);

        // spawn a new thread to run the node else it will block the console
        thread::spawn(move || {
            task::spawn(async move {
                run(state_sender).await;
            });
        });

        // run the console app in the foreground, passing the receiver
        console::run(state_receiver).unwrap();
    } else {
        // TODO: fix default log level and occasionally print state to the console
        env_logger::init();
        run(state_sender).await;
    }
}

pub async fn run(state_sender: crossbeam_channel::Sender<AppState>) {
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
    // derive node identity
    let keys = wallet.keypair;
    let node_id = wallet.node_id;

    // derive genesis piece
    let genesis_piece = crypto::genesis_piece_from_seed("SUBSPACE");
    let genesis_piece_hash = crypto::digest_sha_256(&genesis_piece);

    // create the randomness tracker
    let epoch_tracker = if node_type == NodeType::Gateway {
        EpochTracker::new_genesis(u64::MAX / PLOT_SIZE as u64)
    } else {
        EpochTracker::new()
    };

    // create the ledger
    let (merkle_proofs, merkle_root) = crypto::build_merkle_tree();
    let tx_payload = crypto::generate_random_piece().to_vec();
    let ledger = Ledger::new(
        merkle_root,
        genesis_piece_hash,
        keys,
        tx_payload,
        merkle_proofs,
        epoch_tracker.clone(),
    );

    let is_farming = matches!(node_type, NodeType::Gateway | NodeType::Farmer);

    // create channels between background tasks
    let (any_to_main_tx, any_to_main_rx) = channel::<ProtocolMessage>(32);
    let (timer_to_farmer_tx, timer_to_farmer_rx) = channel::<FarmerMessage>(32);
    let solver_to_main_tx = any_to_main_tx.clone();
    let main_to_main_tx = any_to_main_tx.clone();

    let network_fut = Network::new(
        node_id,
        if node_type == NodeType::Gateway {
            DEV_GATEWAY_ADDR.parse().unwrap()
        } else {
            node_addr
        },
    );
    let network = network_fut.await.unwrap();
    if node_type != NodeType::Gateway {
        info!("Connecting to gateway node");

        network
            .connect_to(DEV_GATEWAY_ADDR.parse().unwrap())
            .await
            .unwrap();
    }

    // manager loop
    let main = manager::run(
        node_type,
        genesis_piece_hash,
        ledger,
        any_to_main_rx,
        network,
        main_to_main_tx,
        state_sender,
        timer_to_farmer_tx,
        epoch_tracker,
    );

    if is_farming {
        // plot, slow...
        let plot = plotter::plot(path.into(), node_id, genesis_piece).await;
        // start solve loop
        let farmer = farmer::run(timer_to_farmer_rx, solver_to_main_tx, &plot);

        join!(main, farmer);
    } else {
        // listen and farm
        join!(main);
    }
}
