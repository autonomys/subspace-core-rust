#![allow(dead_code)]

use async_std::sync::channel;
use async_std::task;
use futures::join;
use log::*;
use manager::ProtocolMessage;
use network::NodeType;
use std::env;
use std::net::SocketAddr;
use subspace_core_rust::*;

/* ToDo
 *
 * Just build something that works
 * Then bench it over a live network
 * Then stress test for known attacks
 * Then get it production ready
 *
 * Implement a basic tui console
 *
 * Base piece audits on block height and piece index correctly
 * Refactor audits / reads to use piece indcies instead of hashes throughout (map arch)
 * Determine what needs to be done to support forks in the ledger
 * Compare quality to target based on size, not leading zeros
 * Implement difficulty threshold correctly
 * Implement a timeout based on deadlines
 *
 * Security Experiments
 *
 * Ensure that block and tx signatures are not malleable
 * Ensure that an attacker cannot crash a node by intentionally creating a panic condition
 * No way to malleate on the work difficulty threshold
 * Run security simulations
 *
 * Production Ready Tasks
 *
 * CUDA plotter
 * Secure wallet implementation
 * Add a notion of transactions
 * Erasure code state, build the state chain, light client syc
 *
 *
 *
*/

fn main() {
    /*
     * Startup: cargo run <node_type> <custom_path>
     *
     * arg1 type -> gateway, farmer, peer (gateway default)
     * arg2 path -> unique path for plot (data_local_dir default)
     *
     * Later: plot size, env
     *
     */

    env_logger::init();

    let node_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mode: NodeType;
    let args: Vec<String> = env::args().collect();
    let _node_type = match args.get(1) {
        Some(_node_type) => {
            match &_node_type[..] {
                "peer" => mode = NodeType::Peer,
                "farmer" => mode = NodeType::Farmer,
                "gateway" => mode = NodeType::Gateway,
                _ => mode = NodeType::Gateway,
            };
        }
        None => mode = NodeType::Gateway,
    };

    info!("Starting new Subspace {:?}", mode);

    // derive node identity
    let keys = crypto::gen_keys_random();
    let binary_public_key: [u8; 32] = keys.public.to_bytes();
    let node_id = crypto::digest_sha_256(&binary_public_key);

    // derive genesis piece
    let genesis_piece = crypto::genesis_piece_from_seed("SUBSPACE");
    let genesis_piece_hash = crypto::digest_sha_256(&genesis_piece);

    // create the ledger
    let (merkle_proofs, merkle_root) = crypto::build_merkle_tree();
    let tx_payload = crypto::generate_random_piece().to_vec();
    let mut ledger = ledger::Ledger::new(merkle_root, genesis_piece_hash);

    // create channels between background tasks
    let (main_to_net_tx, main_to_net_rx) = channel::<ProtocolMessage>(32);
    let (main_to_sol_tx, main_to_sol_rx) = channel::<ProtocolMessage>(32);
    let (any_to_main_tx, any_to_main_rx) = channel::<ProtocolMessage>(32);
    let sol_to_main_tx = any_to_main_tx.clone();

    // only plot/solve if gateway or farmer
    if mode == NodeType::Farmer || mode == NodeType::Gateway {
        task::block_on(async move {
            // plot space (slow...)
            let mut plot = plotter::plot(node_id, genesis_piece).await;

            // init solve loop
            task::spawn(async move {
                solver::run(main_to_sol_rx, sol_to_main_tx, &mut plot).await;
            });
        });
    }

    // manager loop
    let main = task::spawn(async move {
        manager::run(
            mode,
            genesis_piece_hash,
            binary_public_key,
            keys,
            merkle_proofs,
            tx_payload,
            &mut ledger,
            any_to_main_rx,
            main_to_net_tx,
            main_to_sol_tx,
        )
        .await;
    });

    // network loop
    let net = task::spawn(async move {
        network::run(mode, node_id, node_addr, any_to_main_tx, main_to_net_rx).await;
    });

    // join threads
    task::block_on(async move {
        join!(main, net);
    });
}
