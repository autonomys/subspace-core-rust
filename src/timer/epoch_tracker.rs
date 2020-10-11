use crate::timer::Epoch;
use crate::{ProofId, CHALLENGE_LOOKBACK_EPOCHS, EPOCH_CLOSE_WAIT_TIME};
use async_std::sync::Mutex;
use log::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    current_epoch: u64,
    epochs: HashMap<u64, Epoch>,
}

impl Inner {
    fn advance_epoch(&mut self) -> u64 {
        if self.epochs.is_empty() {
            self.current_epoch = 0;
        } else {
            self.current_epoch += 1;
        }

        // Create new epoch
        let current_epoch = self.current_epoch;
        self.epochs.insert(current_epoch, Epoch::new(current_epoch));

        // Close epoch at lookback offset if it exists
        if current_epoch >= EPOCH_CLOSE_WAIT_TIME {
            let close_epoch_index = current_epoch - EPOCH_CLOSE_WAIT_TIME;
            let epoch = self.epochs.get_mut(&close_epoch_index).unwrap();

            epoch.close(current_epoch);

            debug!(
                "Closed epoch with index {}, randomness is {}",
                current_epoch - EPOCH_CLOSE_WAIT_TIME,
                &hex::encode(epoch.randomness)[0..8]
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

    // TODO: does new and new_genesis still need to be seperate?
    pub fn new_genesis() -> Self {
        let inner = Inner::default();

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
    pub async fn add_block_to_epoch(&self, epoch_index: u64, block_height: u64, proof_id: ProofId) {
        let mut inner = self.inner.lock().await;

        if inner.epochs.is_empty() {
            inner.advance_epoch();
        }

        inner
            .epochs
            .get_mut(&epoch_index)
            .unwrap()
            .add_block_at_height(block_height, proof_id);
    }
}
