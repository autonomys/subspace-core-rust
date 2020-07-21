use async_std::sync::channel;
use async_std::task;
use futures::join;
use manager::ProtocolMessage;
use subspace_core_rust::*;

/* ToDo
 * 
 * Overview
 * 
 * First get something that works
 * Then measure its performance
 * Then stress test it against attacks
 * Then build out the remaining features for mvp
 * 
 * 
 * Basic Functionality
 * 
 * Ensure self-generated blocks are valid
 * Reread ledger and solver code for understanding
 * Base piece audits on block height and piece index correctly
 * Determine what needs to be done to support forks in the ledger
 * Create protocol spec, ensure code matches
 * Decide on a transport layer for network and implement
 * Implement a timeout based on deadlines
 * Implement difficulty threshold correctly
 * 
 * Security
 * 
 * Ensure that block and tx signatures are not malleable
 * Ensure that an attacker cannot crash a node by intentionally creating a panic condition
 * No way to malleate on the work difficulty threshold
 * Run security simulations
 * 
 * Production Ready
 * 
 * CUDA plotter
 * Secure wallet implementation
 * Add a notion of transactions
 * Erasure code state, build the state chain, light client syc
 * 
 * Later
 * 
 * Implement with GHOST
 * OpenCL Plotter (based on ff-cl-gen / ff)
 * 
 *
*/

#[async_std::main]
async fn main() {
    println!("Starting new Subspace Network Node");

    // derive node identity
    let keys = crypto::gen_keys();
    let binary_public_key: [u8; 32] = keys.public.to_bytes();
    let node_id = crypto::digest_sha_256(&binary_public_key);

    // derive genesis piece
    let genesis_piece = crypto::genesis_piece_from_seed("SUBSPACE");
    let genesis_piece_hash = crypto::digest_sha_256(&genesis_piece);

    // create the plot (slow...)
    let mut plot = plotter::plot(node_id, genesis_piece).await;

    println!("Finished plotting");

    // setup the solve loop values
    let wait_time: u64 = 1000; // solve wait time in milliseconds
    let (merkle_proofs, merkle_root) = crypto::build_merkle_tree();
    let quality_threshold: u8 = 0;
    let tx_payload = crypto::generate_random_piece().to_vec();

    // create the ledger
    let mut ledger = ledger::Ledger::new(merkle_root, genesis_piece_hash, quality_threshold);

    // create channels between background tasks
    let (main_to_sol_tx, main_to_sol_rx) = channel::<ProtocolMessage>(32);
    let (any_to_main_tx, any_to_main_rx) = channel::<ProtocolMessage>(32);
    let sol_to_main_tx = any_to_main_tx.clone();

    // solve loop
    let solve = task::spawn(async move {
        solver::run(wait_time, main_to_sol_rx, sol_to_main_tx, &mut plot).await;
    });

    // manager loop
    let main = task::spawn(async move {
        manager::run(
            genesis_piece_hash,
            quality_threshold,
            binary_public_key,
            keys,
            merkle_proofs,
            tx_payload,
            &mut ledger,
            any_to_main_rx,
            main_to_sol_tx,
        )
        .await;
    });

    // network loop

    // join threads
    join!(main, solve);
}
