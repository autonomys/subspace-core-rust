use crate::timer::Epoch;
use crate::BlockId;
use crate::CHALLENGE_LOOKBACK;
use crate::EPOCH_CLOSE_WAIT_TIME;
use async_std::sync::Mutex;
use log::warn;
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

    /*
     * Start at Epoch 0
     * Add a genesis block
     * Move to Epoch 1
     * Add a genesis block
     * Move to Epoch 2
     * Close Epoch 0 -> get randomness
     * Close Epoch 2
     * Apply blocks for epoch 2
     *
     * Genesis blocks (working)
     * Create an apply local blocks (buggy code) not ledger, epoch tracker
     * Sync blocks (working)
     * Receive and apply remote blocks (working)
     *
     */

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

            warn!(
                "Closed epoch with index {}, randomness is {}",
                current_epoch - EPOCH_CLOSE_WAIT_TIME,
                &hex::encode(epoch.randomness)[0..8]
            );

            // warn!("{:?}", epoch);
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
}
