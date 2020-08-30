#![allow(dead_code)]

use super::*;
use async_std::prelude::*;
use async_std::stream;
use async_std::sync::Sender;
use log::*;
use manager::ProtocolMessage;
use std::time::Duration;

/* ToDo
 *
 * Remove duplication of epoch/timeslot tracking between ledger and timer
 *
*/

pub type EpochTracker = Arc<Mutex<HashMap<u64, Epoch>>>;

// pub struct EpochTracker ( Arc<Mutex<HashMap<u64, Epoch>>>);

// impl EpochTracker {
//     pub async fn get(&self, current) {

//     }
// }

#[derive(Debug, Clone)]
pub struct Epoch {
    pub is_closed: bool, // has the randomness been derived and the epoch closed?
    pub slots: HashMap<u64, Vec<BlockId>>, // slot indices and vec of block ids, some will be empty, some one, some many
    pub challenges: Vec<SlotChallenge>, // challenges derived from randomness at closure, one per slot
    pub randomness: EpochChallenge,     // overall randomness for this epoch
}

// TODO: Make into an enum for a cleaner implementation, seperate into active and closed epoch
impl Epoch {
    pub fn new(index: u64) -> Epoch {
        let randomness = crypto::digest_sha_256(&index.to_le_bytes());

        Epoch {
            is_closed: false,
            slots: HashMap::new(),
            challenges: Vec::new(),
            randomness,
        }
    }

    pub fn close(&mut self) {
        let xor_result =
            self.slots
                .values()
                .flatten()
                .fold([0u8; 32], |mut randomness, block_id| {
                    utils::xor_bytes(&mut randomness, &block_id[..]);
                    randomness
                });
        self.randomness = crypto::digest_sha_256(&xor_result);

        for timeslot in 0..TIMESLOTS_PER_EPOCH {
            let slot_seed = [&self.randomness[..], &timeslot.to_le_bytes()[..]].concat();
            self.challenges.push(crypto::digest_sha_256(&slot_seed));
        }

        self.is_closed = true;
    }
}

pub async fn run(
    timer_to_solver_tx: Sender<ProtocolMessage>,
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

    // TODO: Make this into a method on epoch_tracker
    let mut epoch = epoch_tracker
        .lock()
        .await
        .get(&(current_epoch_index - CHALLENGE_LOOKBACK))
        .unwrap()
        .clone();

    // advance through timeslot on set interval
    let mut interval = stream::interval(Duration::from_millis(TIMESLOT_DURATION as u64));
    while let Some(_) = interval.next().await {
        info!("Timer has arrived on timeslot: {}", current_timeslot_index);

        if !epoch.is_closed {
            panic!(
                "Epoch {} being used for randomness is still open!",
                current_epoch_index - CHALLENGE_LOOKBACK
            );
        }

        let timeslot_index = current_timeslot_index % TIMESLOTS_PER_EPOCH;

        if is_farming {
            // derive slot challenge and send to solver

            // TODO: make this into a method on epoch
            let slot_challenge = epoch.challenges[timeslot_index as usize];

            timer_to_solver_tx
                .send(ProtocolMessage::SlotChallenge {
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
                epoch = epoch_tracker
                    .lock()
                    .await
                    .get(&(current_epoch_index - CHALLENGE_LOOKBACK))
                    .unwrap()
                    .clone();
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
