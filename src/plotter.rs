#![allow(dead_code)]

use super::*;
use indicatif::ProgressBar;
use rayon::prelude::*;
use std::env;
use std::path::Path;
use std::time::Instant;

/* ToDo
 * Print Sloth Art (Nazar)
 * Read drives and free disk space (sysinfo)
 * Accept user input
 * Show stats when done: time, space, throughput
 * prevent computer from sleeping (enigo)
*/

const PLOT_SIZE: usize = 256 * 100;

pub fn plot() {
    // set storage path
    let path: String;
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        let storage_path = args[1].parse::<String>().unwrap();
        path = Path::new(&storage_path)
            .join("plot.bin")
            .to_str()
            .unwrap()
            .to_string();
    } else {
        path = Path::new(".")
            .join("results")
            .join("plot.bin")
            .to_str()
            .unwrap()
            .to_string();
    }

    // init plotter
    let mut plot = plot::Plot::new(path, PLOT_SIZE);

    // generate random seed data
    let iv = crypto::random_bytes_32();
    let expanded_iv = crypto::expand_iv(iv);

    let mut pieces: Vec<Piece> = vec![];
    for _ in 0..PLOT_SIZE {
        let piece = crypto::generate_random_piece();
        pieces.push(piece);
    }

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

    println!("\nPlotting {} pieces!\n", PLOT_SIZE);

    pieces.par_iter_mut().for_each(|piece| {
        *piece = sloth.encode(piece, expanded_iv, layers);
        bar.inc(1);
    });

    bar.finish();

    pieces.iter().enumerate().for_each(|(i, piece)| {
        plot.add(&piece, i);
    });

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
