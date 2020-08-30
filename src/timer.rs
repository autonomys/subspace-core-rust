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
 * Remove duplication of epoch/timeslot tracking between ledger and timer
 *
*/

pub async fn run(
    timer_to_solver_tx: Sender<SolverMessage>,
    epoch_tracker: EpochTracker,
    initial_epoch_index: u64,
    initial_timeslot_index: u64,
    is_farming: bool,
) {
    // set initial values
    let mut current_epoch_index = initial_epoch_index;
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
    let mut interval = stream::interval(Duration::from_millis(TIMESLOT_DURATION as u64));
    while interval.next().await.is_some() {
        info!("Timer has arrived on timeslot: {}", current_timeslot_index);
        // TODO: Make this into a method on epoch_tracker
        let epoch = epoch_tracker
            .lock()
            .await
            .get(&(current_epoch_index - CHALLENGE_LOOKBACK))
            .unwrap()
            .clone();

        if !epoch.is_closed {
            panic!(
                "Epoch {} being used for randomness is still open!",
                current_epoch_index - CHALLENGE_LOOKBACK
            );
        }

        if is_farming {
            let timeslot_index = current_timeslot_index % TIMESLOTS_PER_EPOCH;

            // derive slot challenge and send to solver

            // TODO: make this into a method on epoch
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
            if is_farming {
                // get the new epoch
                info!(
                    "Getting randomness for epoch: {}",
                    current_epoch_index - CHALLENGE_LOOKBACK
                );
                // TODO: Handle edge case where messages are delayed
                // NOTE: Code for getting epoch was already moved to the beginning on while loop,
                // so above TODO may be irrelevant
            }

            let old_epoch = current_epoch_index;
            current_epoch_index += 1;

            // create the next epoch
            // TODO: create method extend on epoch_tracker
            let next_epoch = Epoch::new(current_epoch_index);
            epoch_tracker
                .lock()
                .await
                .insert(current_epoch_index, next_epoch);

            info!(
                "Timer is creating a new empty epoch at epoch index {}",
                current_epoch_index
            );

            let epoch_tracker_clone = Arc::clone(&epoch_tracker);
            async_std::task::spawn(async move {
                // wait for grace period
                async_std::task::sleep(Duration::from_millis(EPOCH_GRACE_PERIOD)).await;

                // get epoch from tracker and close
                // TODO: turn into method on epoch_tracker: close_epoch(epoch_index)
                epoch_tracker_clone
                    .lock()
                    .await
                    .entry(old_epoch)
                    .and_modify(|epoch| epoch.close());

                info!("Timer is closing randomness for epoch: {}", old_epoch);

                // TODO: remove the expired epoch so that tracker does not grow unbounded
            });
        }
    }
}
