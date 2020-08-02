#![allow(dead_code)]

use super::*;
use crate::plot::Plot;
use async_std::task;
use futures::channel::{mpsc, oneshot};
use futures::{SinkExt, StreamExt};
// use indicatif::ProgressBar;
use async_std::path::PathBuf;
use log::*;
use rayon::prelude::*;
use rug::integer::Order;
use rug::Integer;
use std::thread;
use std::time::Instant;

/* ToDo
 *
 * -- Functionality --
 *
 *
 * -- Polish --
 * Read drives and free disk space (sysinfo)
 * Accept user input
 * prevent computer from sleeping (enigo)
 *
*/

pub async fn plot(path: PathBuf, node_id: NodeID, genesis_piece: Piece) -> Plot {
    info!("New plot initialized at {:?}", path.to_str());

    // channel to send plot writes to an async background task so disk is not blocking
    let (plot_piece_sender, plot_piece_receiver) = mpsc::channel::<(Piece, usize)>(10);

    // channel to send plot back once plotting is complete
    let (plot_sender, plot_receiver) = oneshot::channel::<Plot>();

    // background taks for adding pieces to plot
    thread::spawn(move || {
        let mut plot_piece_receiver = plot_piece_receiver;
        task::block_on(async move {
            // init plot
            let mut plot = Plot::new(&path, PLOT_SIZE).await.unwrap();

            // receive encodings as they are made
            while let Some((piece, index)) = plot_piece_receiver.next().await {
                plot.write(&piece, index).await.unwrap();
            }

            // save the plot map to disk
            plot.force_write_map().await.unwrap();

            // return the plot
            let _ = plot_sender.send(plot);
        });
    });

    let expanded_iv = crypto::expand_iv(node_id);
    let integer_expanded_iv = Integer::from_digits(&expanded_iv, Order::Lsf);
    let piece = genesis_piece;

    // init sloth
    let prime_size = PRIME_SIZE_BITS;
    let layers = ENCODING_LAYERS_TEST;
    let sloth = sloth::Sloth::init(prime_size);

    // let bar = ProgressBar::new(PLOT_SIZE as u64);
    let plot_time = Instant::now();

    info!("Sloth is slowly plotting {} pieces...", PLOT_SIZE);
    // println!("Sloth is slowly plotting {} pieces...", PLOT_SIZE);
    // println!(
    //     r#"
    //       `""==,,__
    //         `"==..__"=..__ _    _..-==""_
    //              .-,`"=/ /\ \""/_)==""``
    //             ( (    | | | \/ |
    //              \ '.  |  \;  \ /
    //               |  \ |   |   ||
    //          ,-._.'  |_|   |   ||
    //         .\_/\     -'   ;   Y
    //        |  `  |        /    |-.
    //        '. __/_    _.-'     /'
    //               `'-.._____.-'
    //     "#
    // );

    // plot pieces in parallel on all cores, using IV as a source of randomness
    // this is just for effecient testing atm
    (0..PLOT_SIZE).into_par_iter().for_each(|index| {
        let mut piece = piece;

        // xor first 16 bytes of piece with the index to get a unqiue piece for each iteration
        let index_bytes = utils::usize_to_bytes(index);
        for i in 0..16 {
            piece[i] = piece[i] ^ index_bytes[i];
        }

        sloth
            .encode(&mut piece, &integer_expanded_iv, layers)
            .unwrap();
        task::block_on(plot_piece_sender.clone().send((piece, index))).unwrap();
        // bar.inc(1);
    });

    drop(plot_piece_sender);

    // bar.finish();

    let total_plot_time = plot_time.elapsed();
    let average_plot_time =
        (total_plot_time.as_nanos() / PLOT_SIZE as u128) as f32 / (1000f32 * 1000f32);

    info!("Average plot time is {:.3} ms per piece", average_plot_time);

    info!(
        "Total plot time is {:.3} minutes",
        total_plot_time.as_secs_f32() / 60f32
    );

    info!(
        "Plotting throughput is {} mb/sec\n",
        ((PLOT_SIZE as u64 * PIECE_SIZE as u64) / (1000 * 1000)) as f32
            / (total_plot_time.as_secs_f32())
    );

    plot_receiver.await.unwrap()
}
