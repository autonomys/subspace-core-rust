use crate::timer::Epoch;
use crate::{
    BlockId, CHALLENGE_LOOKBACK, EPOCH_CLOSE_WAIT_TIME, TIMESLOTS_PER_EON, TIMESLOTS_PER_EPOCH,
};
use async_std::sync::Mutex;
use log::debug;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    current_epoch: u64,
    epochs: HashMap<u64, Epoch>,
}

#[derive(Default, Clone)]
pub struct EpochTracker(Arc<Mutex<Inner>>);

impl EpochTracker {
    pub(super) async fn get_current_epoch(&self) -> u64 {
        self.0.lock().await.current_epoch
    }

    pub async fn get_epoch(&self, epoch_index: u64) -> Epoch {
        self.0
            .lock()
            .await
            .epochs
            .get(&epoch_index)
            .unwrap()
            .clone()
    }

    pub async fn get_lookback_epoch(&self, epoch_index: u64) -> Epoch {
        self.get_epoch(epoch_index - CHALLENGE_LOOKBACK).await
    }

    /// Move to the next epoch
    ///
    /// Returns current epoch index
    pub async fn advance_epoch(&self) -> u64 {
        let mut inner = self.0.lock().await;
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
            let epoch = inner
                .epochs
                .get_mut(&(current_epoch - EPOCH_CLOSE_WAIT_TIME))
                .unwrap();

            epoch.close();

            debug!(
                "Closed epoch with index {}, randomness is {}",
                current_epoch - EPOCH_CLOSE_WAIT_TIME,
                &hex::encode(epoch.randomness)[0..8]
            );
        }

        current_epoch
    }

    /// Returns `true` in case no blocks for this timeslot existed before
    pub async fn add_block_to_epoch(&self, epoch_index: u64, timeslot: u64, block_id: BlockId) {
        self.0
            .lock()
            .await
            .epochs
            .get_mut(&epoch_index)
            .unwrap()
            .add_block_to_timeslot(timeslot, block_id);
    }

    pub async fn get_blocks_for_eon(&self, last_timeslot: u64) -> u64 {
        let mut block_count = 0;

        let first_timeslot = last_timeslot - TIMESLOTS_PER_EON + 1;
        let first_epoch = first_timeslot / TIMESLOTS_PER_EPOCH;
        let mut last_epoch = last_timeslot / TIMESLOTS_PER_EPOCH;

        // 0 Genesis Epoch
        // 1 First Epoch Begins
        // 2048 First Epoch Ends (last timeslot)
        // 2049 Second Epoch Begins

        while last_epoch > first_epoch {
            block_count += self
                .0
                .lock()
                .await
                .epochs
                .get(&last_epoch)
                .unwrap()
                .clone()
                .block_count;
            last_epoch -= 1;
        }

        block_count
    }
}
