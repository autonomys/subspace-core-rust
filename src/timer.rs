mod epoch;
mod epoch_tracker;

use crate::farmer::FarmerMessage;
pub use crate::timer::epoch::Epoch;
pub use crate::timer::epoch_tracker::EpochTracker;
use crate::{CHALLENGE_LOOKBACK, TIMESLOTS_PER_EON, TIMESLOTS_PER_EPOCH, TIMESLOT_DURATION};
use async_std::sync::Sender;
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
                current_epoch_index - CHALLENGE_LOOKBACK
            );
        }

        if is_farming {
            let slot_challenge = epoch.get_challenge_for_timeslot(next_timeslot);
            timer_to_farmer_tx
                .send(FarmerMessage::SlotChallenge {
                    epoch: current_epoch_index,
                    timeslot: next_timeslot,
                    epoch_randomness: epoch.randomness,
                    slot_challenge,
                })
                .await;
        }

        // if the eon has ticked
        // count the number of blocks in the previous eon
        // adjust the solution range accordingly

        next_timeslot += 1;

        if next_timeslot % TIMESLOTS_PER_EON == 0 {
            let block_count = epoch_tracker.get_blocks_for_eon(next_timeslot).await;
            let multiplier = block_count / TIMESLOTS_PER_EON;

            // make sure to account for the lookback parameter

            // for each new eon we compute the multiplier
            // this is passed to farmer who solves on the new multiplier (after the lookback)
            // we then add the multiplier to the block so validation can be correct
            // but how do we know the multiplier is accurate in block?
            // ledger/manager also needs acces to the difficulty

            // where is this checked?
            // when validating the block
            // when solving the block challenge
        }
    }
}
