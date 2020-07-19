#![allow(dead_code)]

use super::*;
use crate::plot::Plot;
use async_std::task;
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use indicatif::ProgressBar;
use rayon::prelude::*;
use rug::integer::Order;
use rug::Integer;
use std::ops::Deref;
use std::path::Path;
use std::time::Instant;
use std::{env, thread};

/* ToDo
 * Print Sloth Art (Nazar)
 * Read drives and free disk space (sysinfo)
 * Accept user input
 * Show stats when done: time, space, throughput
 * prevent computer from sleeping (enigo)
*/

const PLOT_SIZE: usize = 256 * 100;

pub fn plot() {
    let args: Vec<String> = env::args().collect();
    // set storage path
    let path = match args.get(1) {
        Some(path) => Path::new(path).to_path_buf(),
        None => dirs::data_local_dir()
            .expect("Can't find local data directory, needs to be specified explicitly")
            .join("subspace")
            .join("results"),
    };

    let (plot_piece_sender, plot_piece_receiver) = mpsc::channel::<(Piece, usize)>(10);

    thread::spawn(move || {
        let mut plot_piece_receiver = plot_piece_receiver;
        task::block_on(async move {
            // init plotter
            let mut plot = Plot::new(path.deref().into(), PLOT_SIZE).await.unwrap();
            while let Some((piece, index)) = plot_piece_receiver.next().await {
                plot.write(&piece, index).await.unwrap();
            }
            plot.force_write_map().await.unwrap();
        });
    });

    // generate random seed data
    let iv = crypto::random_bytes_32();
    let expanded_iv = crypto::expand_iv(iv);
    let integer_expanded_iv = Integer::from_digits(&expanded_iv, Order::Lsf);

    let piece = crypto::generate_random_piece();

    // init sloth
    let prime_size = 256;
    let layers = PIECE_SIZE / (prime_size / 8);
    let sloth = sloth::Sloth::init(256);

    let bar = ProgressBar::new(PLOT_SIZE as u64);
    let plot_time = Instant::now();

    // let mut piece = crypto::generate_random_piece();

    // for i in 0..PLOT_SIZE {
    //   let encoding = sloth.encode(&mut pieces[i], expanded_iv, layers);
    //   plot.add(&encoding, i);
    //   bar.inc(1);
    // }

    println!("\nPlotting {} pieces!", PLOT_SIZE);
    println!(
        r#"
          `""==,,__
            `"==..__"=..__ _    _..-==""_
                 .-,`"=/ /\ \""/_)==""``
                ( (    | | | \/ |
                 \ '.  |  \;  \ /
                  |  \ |   |   ||
             ,-._.'  |_|   |   ||
            .\_/\     -'   ;   Y
           |  `  |        /    |-.
           '. __/_    _.-'     /'
                  `'-.._____.-'
        "#
    );

    (0..PLOT_SIZE).into_par_iter().for_each(|index| {
        let mut piece = piece;
        sloth
            .encode(&mut piece, &integer_expanded_iv, layers)
            .unwrap();
        task::block_on(plot_piece_sender.clone().send((piece, index))).unwrap();
        bar.inc(1);
    });

    bar.finish();

    let total_plot_time = plot_time.elapsed();
    let average_plot_time =
        (total_plot_time.as_nanos() / PLOT_SIZE as u128) as f32 / (1000f32 * 1000f32);

    println!(
        "\nAverage plot time is {:.3} ms per piece",
        average_plot_time
    );

    println!(
        "Total plot time is {:.3} minutes",
        total_plot_time.as_secs_f32() / 60f32
    );

    println!(
        "Plotting throughput is {} mb/sec\n",
        ((PLOT_SIZE as u64 * PIECE_SIZE as u64) / (1000 * 1000)) as f32
            / (total_plot_time.as_secs_f32())
    );
}
