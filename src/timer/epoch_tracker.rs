use crate::timer::Epoch;
use crate::{
    BlockId, CHALLENGE_LOOKBACK_EPOCHS, DIFFICULTY_LOOKBACK_EONS, EON_CLOSE_WAIT_TIME,
    EPOCHS_PER_EON, EPOCH_CLOSE_WAIT_TIME,
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

#[derive(Default, Clone)]
pub struct EpochTracker {
    inner: Arc<Mutex<Inner>>,
}

impl EpochTracker {
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
        let mut inner = self.inner.lock().await;
        if inner.epochs.is_empty() {
            inner.current_epoch = 0;
        } else {
            inner.current_epoch += 1;
        }

        // Create new epoch
        let current_epoch = inner.current_epoch;
        inner
            .epochs
            .insert(current_epoch, Epoch::new(current_epoch));

        // Close epoch at lookback offset if it exists
        if current_epoch >= EPOCH_CLOSE_WAIT_TIME {
            let close_epoch_index = current_epoch - EPOCH_CLOSE_WAIT_TIME;
            let epoch = inner.epochs.get_mut(&close_epoch_index).unwrap();

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
                .map(|epoch_index| inner.epochs.get(&epoch_index).unwrap().get_block_count())
                .sum::<u64>();
            let difficulty_eon_index = close_eon_index + DIFFICULTY_LOOKBACK_EONS;
            // Get difficulty of the previous eon (fallback to eon 0 if necessary in case of first
            // few eons)
            let previous_solution_range = inner
                .eon_solution_range
                .get(&difficulty_eon_index.checked_sub(1).unwrap_or(0))
                .expect("No difficulty for previous eon, this should never happen");
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
            let difficulty = SolutionRange {
                block_count,
                solution_range_value,
            };
            inner
                .eon_solution_range
                .insert(close_eon_index + DIFFICULTY_LOOKBACK_EONS, difficulty);

            debug!("Closed an eon, block count is {}", block_count);
        }

        current_epoch
    }

    /// Returns `true` in case no blocks for this timeslot existed before
    pub async fn add_block_to_epoch(&self, epoch_index: u64, timeslot: u64, block_id: BlockId) {
        let mut inner = self.inner.lock().await;
        inner
            .epochs
            .get_mut(&epoch_index)
            .unwrap()
            .add_block_to_timeslot(timeslot, block_id);
        if inner.eon_solution_range.is_empty() {
            // TODO: Fill initial values in `eon_solution_range` with difficulty from the block once
            //  it we have it in there
        }
    }
}
