#![allow(dead_code)]

mod epoch;
mod epoch_tracker;

use super::*;
use crate::farmer::FarmerMessage;
pub use crate::timer::epoch::Epoch;
pub use crate::timer::epoch_tracker::EpochTracker;
use async_std::sync::Sender;
use log::*;
use std::time::{Duration, Instant, UNIX_EPOCH};

/* ToDo
 *
*/

pub async fn run(
    timer_to_farmer_tx: Sender<FarmerMessage>,
    epoch_tracker: EpochTracker,
    initial_timeslot: u64,
    is_farming: bool,
    genesis_timestamp: u64,
) {
    // set initial values
    let mut current_epoch_index = epoch_tracker.get_current_epoch().await;
    let mut current_timeslot = initial_timeslot;

    let genesis_instant =
        Instant::now() - (UNIX_EPOCH.elapsed().unwrap() - Duration::from_millis(genesis_timestamp));

    // advance through timeslots on set interval
    loop {
        async_std::task::sleep(
            (current_timeslot as u32 * Duration::from_millis(TIMESLOT_DURATION))
                .checked_sub(genesis_instant.elapsed())
                .unwrap_or_default(),
        )
        .await;

        warn!("Timer has arrived on timeslot: {}", current_timeslot);
        let epoch = epoch_tracker.get_lookback_epoch(current_epoch_index).await;

        if !epoch.is_closed {
            panic!(
                "Epoch {} being used for randomness is still open!",
                current_epoch_index - CHALLENGE_LOOKBACK
            );
        }

        if is_farming {
            let slot_challenge = epoch.get_challenge_for_timeslot(current_timeslot);
            timer_to_farmer_tx
                .send(FarmerMessage::SlotChallenge {
                    epoch: current_epoch_index,
                    timeslot: current_timeslot,
                    epoch_randomness: epoch.randomness,
                    slot_challenge,
                })
                .await;
        }

        current_timeslot += 1;

        if current_timeslot % TIMESLOTS_PER_EPOCH == 0 {
            current_epoch_index = epoch_tracker.advance_epoch().await;

            // info!(
            //     "Timer is creating a new empty epoch at index {}",
            //     current_epoch_index
            // );
        }
    }
}
