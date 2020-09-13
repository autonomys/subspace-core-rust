use crate::timer::Epoch;
use crate::{
    BlockId, CHALLENGE_LOOKBACK_EPOCHS, EON_CLOSE_WAIT_TIME, EPOCHS_PER_EON, EPOCH_CLOSE_WAIT_TIME,
    SOLUTION_RANGE_LOOKBACK_EONS,
};
use async_std::sync::Mutex;
use log::debug;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Copy, Clone)]
struct SolutionRange {
    block_count: u64,
    solution_range_value: u64,
}

#[derive(Default)]
struct Inner {
    current_epoch: u64,
    epochs: HashMap<u64, Epoch>,
    // TODO: Pruning, probably replace with VecDeque
    eon_solution_range: HashMap<u64, SolutionRange>,
}

impl Inner {
    fn fill_initial_solution_range(&mut self, solution_range: u64) {
        for eon_index in 0..EON_CLOSE_WAIT_TIME {
            self.eon_solution_range.insert(
                eon_index,
                SolutionRange {
                    block_count: 0,
                    solution_range_value: solution_range,
                },
            );
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
        let solution_range_value = self
            .eon_solution_range
            .get(
                &current_eon_index
                    .checked_sub(SOLUTION_RANGE_LOOKBACK_EONS)
                    .unwrap_or(0),
            )
            .expect("No solution range for lookback eon, this should never happen")
            .solution_range_value;

        self.epochs.insert(
            current_epoch,
            Epoch::new(current_epoch, solution_range_value),
        );

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
        if current_epoch >= EPOCHS_PER_EON * EON_CLOSE_WAIT_TIME
            && current_epoch % EPOCHS_PER_EON == 0
        {
            let close_eon_start_epoch_index = current_epoch - EPOCHS_PER_EON * EON_CLOSE_WAIT_TIME;
            let close_eon_index = close_eon_start_epoch_index / EPOCHS_PER_EON;
            // Sum up block count from all epochs in a lookback eon
            let block_count = (close_eon_start_epoch_index..)
                .take(EPOCHS_PER_EON as usize)
                .map(|epoch_index| self.epochs.get(&epoch_index).unwrap().get_block_count())
                .sum::<u64>();
            let solution_range_eon_index = close_eon_index + SOLUTION_RANGE_LOOKBACK_EONS;
            // Get solution range of the previous eon (fallback to eon 0 if necessary in case of first
            // few eons)
            let previous_solution_range = self
                .eon_solution_range
                .get(&solution_range_eon_index.checked_sub(1).unwrap_or(0))
                .expect("No solution range for previous eon, this should never happen");
            // Re-adjust previous solution range based on new block count
            let solution_range_value;
            if block_count > 0 && previous_solution_range.block_count > 0 {
                solution_range_value = (previous_solution_range.solution_range_value as f64
                    * block_count as f64
                    / previous_solution_range.block_count as f64)
                    .round() as u64;
            } else {
                solution_range_value = previous_solution_range.solution_range_value;
            };
            let solution_range = SolutionRange {
                block_count,
                solution_range_value,
            };
            self.eon_solution_range.insert(
                close_eon_index + SOLUTION_RANGE_LOOKBACK_EONS,
                solution_range,
            );

            debug!(
                "Closed an eon, block count is {}, new solution range is {}",
                block_count, solution_range_value
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
        solution_range: u64,
    ) {
        let mut inner = self.inner.lock().await;

        if inner.eon_solution_range.is_empty() {
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
