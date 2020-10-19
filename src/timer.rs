mod epoch;
mod epoch_tracker;
use crate::farmer::FarmerMessage;
use crate::manager::SharedLedger;
use crate::{TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION};
use async_std::sync::Sender;
pub use epoch::Epoch;
pub use epoch_tracker::EpochTracker;
use log::*;
use std::time::{Duration, Instant, UNIX_EPOCH};

pub async fn run(
    timer_to_farmer_tx: Sender<FarmerMessage>,
    epoch_tracker: EpochTracker,
    is_farming: bool,
    genesis_timestamp: u64,
    next_timeslot: u64,
    ledger: SharedLedger,
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

        debug!("Timer has arrived on timeslot: {}", next_timeslot);

        {
            ledger.lock().await.next_timeslot().await;

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
        }

        let (randomness, slot_challenge) = epoch_tracker
            .get_slot_challenge(current_epoch_index, next_timeslot)
            .await;

        if is_farming {
            // TODO: This doesn't wait until we solve, so in case disk is overloaded, this
            //  will cause DoS

            // TODO: this should not take effect until N timeslots after the new range has been calculated
            let solution_range = ledger.lock().await.current_solution_range;

            timer_to_farmer_tx
                .send(FarmerMessage::SlotChallenge {
                    epoch_index: current_epoch_index,
                    timeslot: next_timeslot,
                    randomness,
                    slot_challenge,
                    solution_range,
                })
                .await;
        }

        next_timeslot += 1;
    }
}
