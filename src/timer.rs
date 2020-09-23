mod epoch;
mod epoch_tracker;

use crate::farmer::FarmerMessage;
use crate::{CHALLENGE_LOOKBACK_EPOCHS, TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION};
use async_std::sync::Sender;
pub use epoch::Epoch;
pub use epoch_tracker::EpochTracker;
use log::*;
use std::time::{Duration, Instant, UNIX_EPOCH};

pub async fn run(
    timer_to_farmer_tx: Sender<FarmerMessage>,
    epoch_tracker: EpochTracker,
    next_timeslot: u64,
    is_farming: bool,
    genesis_timestamp: u64,
) {
    info!(
        "Starting timer with genesis timestamp {}...",
        genesis_timestamp
    );

    // set initial values
    let mut current_epoch_index = epoch_tracker.get_current_epoch().await;
    let mut next_timeslot = next_timeslot;

    let genesis_instant =
        Instant::now() - (UNIX_EPOCH.elapsed().unwrap() - Duration::from_millis(genesis_timestamp));

    // advance through timeslots on set interval
    loop {
        async_std::task::sleep(
            (next_timeslot as u32 * Duration::from_millis(TIMESLOT_DURATION))
                .checked_sub(genesis_instant.elapsed())
                .unwrap_or_default(),
        )
        .await;

        // TODO: apply all blocks that were referenced in the previous round (happy path)
        // need a copy of ledger here

        // We are looking to epoch boundary, but also trying not to go ahead of clock
        if next_timeslot % TIMESLOTS_PER_EPOCH == 0
            && (current_epoch_index < next_timeslot / TIMESLOTS_PER_EPOCH)
        {
            current_epoch_index = epoch_tracker.advance_epoch().await;

            debug!(
                "Timer is creating a new empty epoch at index {}",
                current_epoch_index
            );
        }

        debug!("Timer has arrived on timeslot: {}", next_timeslot);
        let epoch = epoch_tracker.get_lookback_epoch(current_epoch_index).await;

        if !epoch.is_closed {
            panic!(
                "Epoch {} being used for randomness is still open!",
                current_epoch_index - CHALLENGE_LOOKBACK_EPOCHS
            );
        }

        if is_farming {
            let slot_challenge = epoch.get_challenge_for_timeslot(next_timeslot);
            // TODO: This doesn't wait until we solve, so in case disk is overloaded, this
            //  will cause DoS
            timer_to_farmer_tx
                .send(FarmerMessage::SlotChallenge {
                    epoch_index: current_epoch_index,
                    timeslot: next_timeslot,
                    randomness: epoch.randomness,
                    slot_challenge,
                    solution_range: epoch.solution_range,
                })
                .await;
        }

        next_timeslot += 1;
    }
}
