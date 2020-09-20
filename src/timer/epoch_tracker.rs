use crate::timer::Epoch;
use crate::{
    BlockId, CHALLENGE_LOOKBACK_EPOCHS, EPOCHS_PER_EON, EPOCH_CLOSE_WAIT_TIME, PIECE_SIZE,
    SOLUTION_RANGE_LOOKBACK_EONS, TIMESLOTS_PER_EPOCH,
};
use async_std::sync::Mutex;
use log::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    current_epoch: u64,
    epochs: HashMap<u64, Epoch>,
    // TODO: Pruning, probably replace with VecDeque
    /// Solution range for an eon (computed from lookback eon when it was closed)
    eon_to_solution_range: HashMap<u64, u64>,
}

impl Inner {
    fn fill_initial_solution_range(&mut self, solution_range: u64) {
        for eon_index in 0..SOLUTION_RANGE_LOOKBACK_EONS {
            self.eon_to_solution_range.insert(eon_index, solution_range);
        }
    }

    fn advance_epoch(&mut self) -> u64 {
        if self.epochs.is_empty() {
            self.current_epoch = 0;
        } else {
            self.current_epoch += 1;
        }

        // Create new epoch
        let current_epoch = self.current_epoch;
        let current_eon_index = current_epoch / EPOCHS_PER_EON;
        // Get solution range of lookback eon (fallback to eon 0 if necessary in case of first few eons)
        let solution_range = *self
            .eon_to_solution_range
            .get(
                &current_eon_index
                    .checked_sub(SOLUTION_RANGE_LOOKBACK_EONS)
                    .unwrap_or(0),
            )
            .expect("No solution range for lookback eon, this should never happen");

        self.epochs
            .insert(current_epoch, Epoch::new(current_epoch, solution_range));

        // Close epoch at lookback offset if it exists
        if current_epoch >= EPOCH_CLOSE_WAIT_TIME {
            let close_epoch_index = current_epoch - EPOCH_CLOSE_WAIT_TIME;
            let epoch = self.epochs.get_mut(&close_epoch_index).unwrap();

            epoch.close();

            debug!(
                "Closed epoch with index {}, randomness is {}",
                current_epoch - EPOCH_CLOSE_WAIT_TIME,
                &hex::encode(epoch.randomness)[0..8]
            );
        }

        // Close an eon
        if current_epoch >= EPOCHS_PER_EON * SOLUTION_RANGE_LOOKBACK_EONS
            && current_epoch % EPOCHS_PER_EON == 0
        {
            let lookback_eon_start_epoch_index =
                current_epoch - EPOCHS_PER_EON * SOLUTION_RANGE_LOOKBACK_EONS;
            let lookback_eon_index = lookback_eon_start_epoch_index / EPOCHS_PER_EON;
            // Sum up block count from all epochs in a lookback eon
            let block_count = (lookback_eon_start_epoch_index..)
                .take(EPOCHS_PER_EON as usize)
                .map(|epoch_index| self.epochs.get(&epoch_index).unwrap().get_block_count())
                .sum::<u64>();
            // Get solution range of the previous eon (fallback to eon 0 if necessary in case of first
            // few eons)
            let lookback_solution_range = *self
                .eon_to_solution_range
                .get(&lookback_eon_index)
                .expect("No solution range for current eon, this should never happen");
            // Re-adjust previous solution range based on new block count
            let solution_range = if block_count > 0 {
                let new_solution_range = (lookback_solution_range as f64 / block_count as f64
                    * (TIMESLOTS_PER_EPOCH * EPOCHS_PER_EON) as f64)
                    .round() as u64;

                // Apply 3% of the change to lookback solution range
                let solution_range = lookback_solution_range as i64
                    + (new_solution_range as i64 - lookback_solution_range as i64) / 100 * 3;

                // Should divide by 2 without remainder
                solution_range as u64 / 2 * 2
            } else {
                lookback_solution_range
            };

            self.eon_to_solution_range.insert(
                lookback_eon_index + SOLUTION_RANGE_LOOKBACK_EONS,
                solution_range,
            );

            let bytes_pledged = (u64::MAX / lookback_solution_range) * PIECE_SIZE as u64;

            info!(
                "Closed an eon, block count is {}, used solution range {}, new solution range is {}, ~{}MiB bytes pledged",
                block_count,
                lookback_solution_range,
                solution_range,
                bytes_pledged / 1024 / 1024,
            );
        }

        current_epoch
    }
}

#[derive(Clone)]
pub struct EpochTracker {
    inner: Arc<Mutex<Inner>>,
}

impl EpochTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::default(),
        }
    }

    pub fn new_genesis(initial_solution_range: u64) -> Self {
        let mut inner = Inner::default();

        inner.fill_initial_solution_range(initial_solution_range);

        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    pub(super) async fn get_current_epoch(&self) -> u64 {
        self.inner.lock().await.current_epoch
    }

    pub async fn get_epoch(&self, epoch_index: u64) -> Epoch {
        self.inner
            .lock()
            .await
            .epochs
            .get(&epoch_index)
            .unwrap()
            .clone()
    }

    pub async fn get_lookback_epoch(&self, epoch_index: u64) -> Epoch {
        self.get_epoch(epoch_index - CHALLENGE_LOOKBACK_EPOCHS)
            .await
    }

    /// Move to the next epoch
    ///
    /// Returns current epoch index
    pub async fn advance_epoch(&self) -> u64 {
        self.inner.lock().await.advance_epoch()
    }

    /// Returns `true` in case no blocks for this timeslot existed before
    pub async fn add_block_to_epoch(
        &self,
        epoch_index: u64,
        timeslot: u64,
        block_id: BlockId,
        // distance_from_challenge: u64,
        solution_range: u64,
    ) {
        let mut inner = self.inner.lock().await;

        if inner.eon_to_solution_range.is_empty() {
            inner.fill_initial_solution_range(solution_range);
            // Create the initial epoch
            inner.advance_epoch();
        }

        inner
            .epochs
            .get_mut(&epoch_index)
            .unwrap()
            .add_block_to_timeslot(timeslot, block_id);
    }
}
