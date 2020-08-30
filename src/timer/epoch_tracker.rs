use crate::timer::Epoch;
use crate::BlockId;
use crate::CHALLENGE_LOOKBACK;
use async_std::sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    epochs: HashMap<u64, Epoch>,
}

#[derive(Default, Clone)]
pub struct EpochTracker(Arc<Mutex<Inner>>);

impl EpochTracker {
    pub async fn get_epoch(&self, epoch_index: u64) -> Epoch {
        self.0
            .lock()
            .await
            .epochs
            .get(&epoch_index)
            .unwrap()
            .clone()
    }

    pub async fn get_loopback_epoch(&self, epoch_index: u64) -> Epoch {
        self.get_epoch(epoch_index - CHALLENGE_LOOKBACK).await
    }

    pub async fn create_epoch(&self, epoch_index: u64) {
        self.0
            .lock()
            .await
            .epochs
            .insert(epoch_index, Epoch::new(epoch_index));
    }

    /// Returns `true` in case no blocks for this timeslot existed before
    pub async fn add_block_to_epoch(
        &self,
        epoch_index: u64,
        timeslot: u64,
        block_id: BlockId,
    ) -> bool {
        let mut new_timeslot = true;
        self.0
            .lock()
            .await
            .epochs
            .entry(epoch_index)
            .and_modify(|epoch| {
                new_timeslot = epoch.add_block_to_timeslot(timeslot, block_id);
            });

        new_timeslot
    }

    pub async fn close_epoch(&self, epoch_index: u64) {
        self.0
            .lock()
            .await
            .epochs
            .entry(epoch_index)
            .and_modify(|epoch| epoch.close());
    }
}
