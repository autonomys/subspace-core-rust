#![allow(dead_code)]

use async_std::sync::channel;
use async_std::task;
use futures::join;
use manager::ProtocolMessage;
use network::NodeType;
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use subspace_core_rust::*;

/* ToDo
 *
 * Just build something that works
 * Then bench it over a live network
 * Then stress test for known attacks
 * Then get it production ready
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

#[async_std::main]
async fn main() {
    println!("Starting new Subspace Network Node");

    let gateway_port: u64 = 8080;
    let gateway_addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();

    let ip = Ipv4Addr::new(127, 0, 0, 1);
    let port: u16;
    let mode: NodeType;

    let args: Vec<String> = env::args().collect();
    let _path = match args.get(1) {
        Some(_path) => {
            port = args[2].parse::<u16>().unwrap();
            mode = NodeType::Peer;
        }
        None => {
            port = gateway_port as u16;
            mode = NodeType::Gateway;
        }
    };

    // derive node identity
    let keys = crypto::gen_keys_from_seed(port as u64);
    let binary_public_key: [u8; 32] = keys.public.to_bytes();
    let node_id = crypto::digest_sha_256(&binary_public_key);
    let node_addr = SocketAddr::new(IpAddr::V4(ip), port);

    // assign stats based on node type

    // derive genesis piece
    let genesis_piece = crypto::genesis_piece_from_seed("SUBSPACE");
    let genesis_piece_hash = crypto::digest_sha_256(&genesis_piece);

    // only plot if gateway or farmer
    // create the plot (slow...)
    let mut plot = plotter::plot(node_id, genesis_piece).await;

    // setup the solve loop values
    let wait_time: u64 = 1000; // solve wait time in milliseconds
    let (merkle_proofs, merkle_root) = crypto::build_merkle_tree();
    let quality_threshold: u8 = 0;
    let tx_payload = crypto::generate_random_piece().to_vec();

    // create the ledger
    let mut ledger = ledger::Ledger::new(merkle_root, genesis_piece_hash, quality_threshold);

    // create channels between background tasks
    let (main_to_net_tx, main_to_net_rx) = channel::<ProtocolMessage>(32);
    let (main_to_sol_tx, main_to_sol_rx) = channel::<ProtocolMessage>(32);
    let (any_to_main_tx, any_to_main_rx) = channel::<ProtocolMessage>(32);
    let sol_to_main_tx = any_to_main_tx.clone();

    // only run if solvingf
    // solve loop
    let solve = task::spawn(async move {
        solver::run(wait_time, main_to_sol_rx, sol_to_main_tx, &mut plot).await;
    });

    // manager loop
    let main = task::spawn(async move {
        manager::run(
            mode,
            genesis_piece_hash,
            quality_threshold,
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
        network::run(
            mode,
            node_id,
            node_addr,
            gateway_addr,
            any_to_main_tx,
            main_to_net_rx,
        )
        .await;
    });

    // join threads
    join!(main, solve, net);
}
