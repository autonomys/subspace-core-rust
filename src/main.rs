use subspace_core_rust::*;

/* ToDo
 * 
 * Store keys and node id in wallet struct
 * 
*/

#[async_std::main]
async fn main() {
    println!("Starting new Subspace Network Node");

    // derive node identity
    let keys = crypto::gen_keys();
    let binary_public_key: [u8; 32] = keys.public.to_bytes();
    let node_id = crypto::digest_sha_256(&binary_public_key);

    // create genesis piece and plot
    let genesis_piece = crypto::genesis_piece_from_seed("SUBSPACE");
    let genesis_piece_hash = crypto::digest_sha_256(&genesis_piece);
    let wait_time: u64 = 1000; // solve wait time in milliseconds

    // plot pieces, return the plot
    let plot = plotter::plot(node_id, genesis_piece).await;

    // start eval loop from seed


}