use crate::timer::Epoch;
use crate::{crypto, ProofId, CHALLENGE_LOOKBACK_EPOCHS, EPOCH_CLOSE_WAIT_TIME};
use async_std::sync::Mutex;
use log::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    current_epoch: u64,
    epochs: HashMap<u64, Epoch>,
}

/*
   Start with a short seed and genesis time
   From the seed
    We first derive the genesis state and plot
    Then we derive the genesis epoch challenge and slot challenges

   At any point we should be able to know time from epoch or timeslot index
   We only need to store the randomness, blocks_at_height, and if_closed in the epoch.
   Challenges can be derived from the randomness on demand

   If get lookback is called on epoch 0 then we return the genesis epoch challenge


*/

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

    pub async fn new_genesis() -> Self {
        let inner = Inner::default();

        let tracker = Self {
            inner: Arc::new(Mutex::new(inner)),
        };

        tracker.advance_epoch().await;
        tracker
    }

    pub(super) async fn get_current_epoch(&self) -> u64 {
        self.inner.lock().await.current_epoch
    }

    async fn get_epoch(&self, epoch_index: u64) -> Epoch {
        self.inner
            .lock()
            .await
            .epochs
            .get(&epoch_index)
            .unwrap()
            .clone()
    }

    pub async fn get_slot_challenge(
        &self,
        epoch_index: u64,
        timeslot: u64,
    ) -> ([u8; 32], [u8; 32]) {
        let randomness: [u8; 32] = if epoch_index == 0 {
            // return the genesis randomness
            // TODO: make this work off the seed
            [0u8; 32]
        } else {
            // get the randomness from the previous closed epoch
            let lookback_epoch = self
                .get_epoch(epoch_index - CHALLENGE_LOOKBACK_EPOCHS)
                .await;

            // TODO: this can cause panic if clocks are even slightly out of sync
            if !lookback_epoch.is_closed {
                panic!(
                    "Epoch {} being used for randomness is still open!",
                    epoch_index - CHALLENGE_LOOKBACK_EPOCHS
                );
            }

            lookback_epoch.randomness
        };

        let slot_challenge =
            crypto::digest_sha_256(&[&randomness[..], &timeslot.to_le_bytes()[..]].concat());
        (randomness, slot_challenge)
    }

    /// Move to the next epoch
    ///
    /// Returns current epoch index
    pub async fn advance_epoch(&self) -> u64 {
        self.inner.lock().await.advance_epoch()
    }

    /// Returns `true` in case no blocks for this timeslot existed before
    pub async fn add_block_to_epoch(&self, epoch_index: u64, block_height: u64, proof_id: ProofId) {
        self.inner
            .lock()
            .await
            .epochs
            .get_mut(&epoch_index)
            .unwrap()
            .add_block_at_height(block_height, proof_id);
    }

    pub async fn remove_block_from_epoch(
        &self,
        epoch_index: u64,
        block_height: u64,
        proof_id: ProofId,
    ) {
        self.inner
            .lock()
            .await
            .epochs
            .get_mut(&epoch_index)
            .unwrap()
            .remove_block_at_height(block_height, proof_id);
    }
}
