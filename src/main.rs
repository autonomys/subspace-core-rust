use async_signals::Signals;
use async_std::sync::channel;
use async_std::task;
use clap::{Clap, ValueHint};
use console::AppState;
use crossbeam_channel::unbounded;
use daemonize_me::Daemon;
use exitcode::{OK, SOFTWARE};
use futures::StreamExt;
use futures_lite::FutureExt;
use log::LevelFilter;
use log::*;
use std::fs;
use std::path::PathBuf;
use std::thread;
use subspace_core_rust::farmer::FarmerMessage;
use subspace_core_rust::ipc::{IpcRequestMessage, IpcResponseMessage, IpcServer};
use subspace_core_rust::ledger::Ledger;
use subspace_core_rust::manager::ProtocolMessage;
use subspace_core_rust::network::{Network, NodeType};
use subspace_core_rust::plot::Plot;
use subspace_core_rust::pseudo_wallet::Wallet;
use subspace_core_rust::timer::EpochTracker;
use subspace_core_rust::{
    console, farmer, ipc, manager, network, plotter, rpc, state, BLOCK_LIST_SIZE, CONSOLE,
    DEV_GATEWAY_ADDR, GENESIS_PIECE_COUNT, IPC_SOCKET_FILE, MAINTAIN_PEERS_INTERVAL, MAX_CONTACTS,
    MAX_PEERS, MIN_CONTACTS, MIN_PEERS,
};
use tui_logger::{init_logger, set_default_level};

/* TODO

 Next Steps

   - complete state encoding workflow
   - improve console UX
   - test transaction throughput
   - finish p2p network impl (Nazar)

   - refactor plot to work of the same method for startup and subsequent plotting
   - refactor sloth into encoder/decoder
   - handle deep forks when there are only two blocks for the same first timeslot

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

#[derive(Debug, Clap)]
#[clap(about, version)]
enum Command {
    /// Run a subspace node
    Run {
        /// Node type to run
        #[clap(arg_enum)]
        node_type: NodeType,
        /// Use custom path for data storage instead of platform-specific default
        #[clap(long, value_hint = ValueHint::FilePath)]
        custom_path: Option<PathBuf>,
        /// Run in the background as daemon
        #[clap(long)]
        daemon: bool,
        /// Run WebSocket RPC server
        #[clap(long)]
        ws_rpc_server: bool,
    },
    /// Stop subspace node that was previously running as a daemon
    Stop {
        /// Use custom path for data storage instead of platform-specific default
        #[clap(long, value_hint = ValueHint::FilePath)]
        custom_path: Option<PathBuf>,
    },
    /// Start TUI to watch a subspace node running
    Watch {
        /// Use custom path for data storage instead of platform-specific default
        #[clap(long, value_hint = ValueHint::FilePath)]
        custom_path: Option<PathBuf>,
    },
    /// Send some credits using node wallet
    Send {
        // TODO: This should probably be a more specific type
        /// Receiver address
        address: String,
        // TODO: This should probably be a more specific type
        /// Amount of credits to send
        amount: u64,
        /// Use custom path for data storage instead of platform-specific default
        #[clap(long, value_hint = ValueHint::FilePath)]
        custom_path: Option<PathBuf>,
    },
}

fn get_path(custom_path: Option<PathBuf>) -> PathBuf {
    // set storage path
    let path = custom_path
        .or_else(|| std::env::var("SUBSPACE_DIR").map(PathBuf::from).ok())
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

    path
}

#[async_std::main]
async fn main() {
    let command: Command = Command::parse();
    match command {
        Command::Run {
            node_type,
            custom_path,
            daemon,
            ws_rpc_server,
        } => {
            let path = get_path(custom_path);
            // TODO: Doesn't really work, see https://github.com/octetd/daemonize-me/issues/2
            if daemon {
                let stdout = std::fs::File::create(path.join("daemon.out")).unwrap();
                let stderr = std::fs::File::create(path.join("daemon.err")).unwrap();
                match Daemon::new()
                    .stdout(stdout)
                    .stderr(stderr)
                    .pid_file(path.join("subspace.pid"), Some(false))
                    .start()
                {
                    Ok(_) => {
                        info!("Node successfully started in background");
                    }
                    Err(error) => {
                        error!("Failed to start node in the background: {}", error);
                        std::process::exit(1);
                    }
                }
            }

            // create an async channel for console
            let (app_state_sender, app_state_receiver) = unbounded::<AppState>();

            if CONSOLE {
                // setup the logger
                init_logger(LevelFilter::Debug).unwrap();
                set_default_level(LevelFilter::Trace);

                // spawn a new thread to run the node else it will block the console
                thread::spawn(move || {
                    task::spawn(async move {
                        run(app_state_sender, node_type, path, ws_rpc_server).await;
                    });
                });

                // run the console app in the foreground, passing the receiver
                console::run(app_state_receiver).unwrap();
            } else {
                // TODO: fix default log level and occasionally print state to the console
                env_logger::init();
                run(app_state_sender, node_type, path, ws_rpc_server).await;
            }
        }
        Command::Stop { custom_path } => {
            let path = get_path(custom_path);
            ipc::simple_request(path.join(IPC_SOCKET_FILE), &IpcRequestMessage::Shutdown)
                .await
                .unwrap();
        }
        Command::Watch { custom_path } => {
            let path = get_path(custom_path);
            unimplemented!();
        }
        Command::Send {
            address,
            amount,
            custom_path,
        } => {
            let path = get_path(custom_path);
            unimplemented!();
        }
    }
}

pub async fn run(
    app_state_sender: crossbeam_channel::Sender<AppState>,
    node_type: NodeType,
    path: PathBuf,
    ws_rpc_server: bool,
) {
    let node_addr = "127.0.0.1:0".parse().unwrap();

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
            plotter::plot(path.clone().into(), node_id, state_bundle).await
        }
        _ => {
            // create an empty plot
            plotter::plot(path.clone().into(), node_id, vec![]).await
        }
    };

    // create the ledger
    let ledger = Ledger::new(keys, epoch_tracker.clone(), state);

    // create the network
    let startup_network_fut = Network::new(
        node_id,
        if node_type == NodeType::Gateway {
            DEV_GATEWAY_ADDR.parse().unwrap()
        } else {
            node_addr
        },
        if node_type == NodeType::Gateway {
            vec![]
        } else {
            vec![DEV_GATEWAY_ADDR.parse().unwrap()]
        },
        &path,
        MIN_PEERS,
        MAX_PEERS,
        MIN_CONTACTS,
        MAX_CONTACTS,
        BLOCK_LIST_SIZE,
        MAINTAIN_PEERS_INTERVAL,
        network::create_backoff,
    );

    // initiate outbound network connections
    let network = startup_network_fut.await.unwrap();

    // optionally create the RPC server
    let mut rpc_server = None;
    if std::env::var("RUN_WS_RPC")
        .map(|value| value == "1".to_string())
        .unwrap_or(ws_rpc_server)
    {
        rpc_server = Some(rpc::run(node_id, network.clone()));
    }

    let (ipc_shutdown_request_sender, mut ipc_shutdown_request_receiver) =
        async_channel::unbounded::<()>();
    let ipc_server_fut = IpcServer::new(
        path.join(IPC_SOCKET_FILE),
        move |(request_message, response_sender)| {
            trace!("Received IPC request {:?}", request_message);

            let response_message = match request_message {
                IpcRequestMessage::Shutdown => {
                    let ipc_shutdown_request_sender = ipc_shutdown_request_sender.clone();
                    async_std::task::spawn(async move {
                        let _ = ipc_shutdown_request_sender.send(()).await;
                    });

                    Ok(IpcResponseMessage::Shutdown)
                }
            };

            trace!("Sending IPC response {:?}", response_message);
            let _ = response_sender.send(response_message);
        },
    );

    let _ipc_server = ipc_server_fut.await.unwrap();

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

    let mut app_handler = Some(async_std::task::spawn({
        let plot = plot.clone();

        async move {
            // create the farming loop
            let farmer_fut = farmer::run(timer_to_farmer_rx, solver_to_main_tx, &plot);

            manager.race(farmer_fut).await;
        }
    }));

    let mut signals = Signals::new(vec![libc::SIGINT, libc::SIGTERM])
        .unwrap()
        .map(|_| ());

    // Wrap into option, such that we can drop Plot when needed without dropping the whole variable
    let mut plot = Some(plot);
    let mut already_shutting_down = false;
    while let Some(_) = ipc_shutdown_request_receiver
        .next()
        .race(signals.next())
        .await
    {
        if already_shutting_down {
            warn!("Received signal second time, force shutdown");
            std::process::exit(SOFTWARE);
        }
        already_shutting_down = true;

        info!("Shutdown signal received, shutting down");
        // Stop RPC server
        rpc_server.take();
        if let Some(app_handler) = app_handler.take() {
            let plot = plot.take().unwrap();
            async_std::task::spawn(async move {
                // TODO: We probably want to do something more sophisticated than just canceling it
                app_handler.cancel().await;

                let (close_sender, close_receiver) = async_oneshot::oneshot::<()>();
                plot.on_close(move || {
                    let _ = close_sender.send(());
                })
                .detach();

                // We wait for plot to close gracefully, otherwise we may get this message:
                // pure virtual method called
                // terminate called without an active exception
                drop(plot);
                let _ = close_receiver.await;

                std::process::exit(OK);
            });
        }
    }
}
