#![allow(dead_code)]

mod epoch;
mod epoch_tracker;

use super::*;
use crate::solver::SolverMessage;
pub use crate::timer::epoch::Epoch;
pub use crate::timer::epoch_tracker::EpochTracker;
use async_std::prelude::*;
use async_std::stream;
use async_std::sync::Sender;
use log::*;
use std::time::Duration;

/* ToDo
 *
 * Remove duplication of timeslot tracking between ledger and timer
 *
*/

pub async fn run(
    timer_to_solver_tx: Sender<SolverMessage>,
    epoch_tracker: EpochTracker,
    initial_timeslot_index: u64,
    is_farming: bool,
) {
    // set initial values
    let mut current_epoch_index = epoch_tracker.get_current_epoch().await;
    let mut current_timeslot_index = initial_timeslot_index;

    // info! {"Initial epoch index is: {}", current_epoch_index};
    // info!("Inital timeslot index is: {}", current_timeslot_index);

    info!(
        "Getting randomness for initial epoch: {}",
        current_epoch_index - CHALLENGE_LOOKBACK
    );

    // get initial epoch for source of randomness
    // this should be a closed epoch so we just clone

    // advance through timeslot on set interval
    let mut interval = stream::interval(Duration::from_millis(TIMESLOT_DURATION));
    while interval.next().await.is_some() {
        info!("Timer has arrived on timeslot: {}", current_timeslot_index);
        let epoch = epoch_tracker.get_loopback_epoch(current_epoch_index).await;

        if !epoch.is_closed {
            panic!(
                "Epoch {} being used for randomness is still open!",
                current_epoch_index - CHALLENGE_LOOKBACK
            );
        }

        if is_farming {
            let timeslot_index = current_timeslot_index % TIMESLOTS_PER_EPOCH;

            // derive slot challenge and send to solver

            let slot_challenge = epoch.get_challenge_for_timeslot(timeslot_index as usize);

            timer_to_solver_tx
                .send(SolverMessage::SlotChallenge {
                    epoch: current_epoch_index,
                    timeslot: current_timeslot_index,
                    epoch_randomness: epoch.randomness,
                    slot_challenge,
                })
                .await;
        }

        current_timeslot_index += 1;

        // update epoch on boundaries
        if current_timeslot_index % TIMESLOTS_PER_EPOCH == 0 {
            // create the next epoch
            current_epoch_index = epoch_tracker.advance_epoch().await;

            info!(
                "Timer is creating a new empty epoch at epoch index {}",
                current_epoch_index
            );
        }
    }
}
