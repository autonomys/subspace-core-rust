use crate::plot::Plot;
use crate::state::PieceBundle;
use crate::{
    crypto, sloth, NodeID, CONSOLE, ENCODING_LAYERS_TEST, PIECE_SIZE, PLOT_SIZE, PRIME_SIZE_BITS,
};
use async_std::path::PathBuf;
use async_std::task;
use indicatif::ProgressBar;
use log::*;
use rayon::prelude::*;
use rug::integer::Order;
use rug::Integer;
use std::convert::TryInto;
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

pub async fn plot(path: PathBuf, node_id: NodeID, piece_bundles: Vec<PieceBundle>) -> Plot {
    // init plot
    let plot = Plot::open_or_create(&path).await.unwrap();

    if plot.is_empty().await {
        let plotting_fut = task::spawn_blocking({
            let plot = plot.clone();

            move || {
                let expanded_iv = crypto::expand_iv(node_id);
                let integer_expanded_iv = Integer::from_digits(&expanded_iv, Order::Lsf);

                // init sloth
                let sloth = sloth::Sloth::init(PRIME_SIZE_BITS);

                let mut bar: Option<ProgressBar> = None;
                if !CONSOLE {
                    bar = Some(ProgressBar::new(PLOT_SIZE as u64))
                };

                // plot pieces in parallel on all cores, using IV as a source of randomness
                // this is just for efficient testing atm
                piece_bundles.into_par_iter().for_each(|piece_bundle| {
                    let mut piece = piece_bundle.piece;

                    sloth
                        .encode(&mut piece, &integer_expanded_iv, ENCODING_LAYERS_TEST)
                        .unwrap();

                    // TODO: Replace challenge here and in other places
                    let nonce = u64::from_le_bytes(
                        crypto::create_hmac(&piece, b"subspace")[0..8]
                            .try_into()
                            .unwrap(),
                    );

                    task::spawn({
                        let plot = plot.clone();

                        async move {
                            let result = plot
                                .write(
                                    piece,
                                    nonce,
                                    piece_bundle.piece_index,
                                    piece_bundle.piece_proof,
                                )
                                .await;

                            if let Err(error) = result {
                                warn!("{}", error);
                            }
                        }
                    });
                    if let Some(b) = &bar {
                        b.inc(1);
                    }
                });

                if let Some(b) = &bar {
                    b.finish();
                }
            }
        });

        let plot_time = Instant::now();

        info!("Sloth is slowly plotting {} pieces...", PLOT_SIZE);

        if !CONSOLE {
            eprintln!(
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
        }

        plotting_fut.await;

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
    } else {
        info!("Using existing plot...");
    }

    plot
}
