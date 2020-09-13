use crate::timer::Epoch;
use crate::{BlockId, CHALLENGE_LOOKBACK_EPOCHS, EPOCHS_PER_EON, EPOCH_CLOSE_WAIT_TIME};
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
        self.get_epoch(epoch_index - CHALLENGE_LOOKBACK_EPOCHS)
            .await
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
        if current_epoch > 0 && current_epoch % EPOCHS_PER_EON == 0 {
            // Sum up block count from all epochs in in eon
            let block_count = (current_epoch - EPOCHS_PER_EON..current_epoch)
                .map(|epoch_index| inner.epochs.get(&epoch_index).unwrap().get_block_count())
                .sum::<u64>();
            // let multiplier = block_count / TIMESLOTS_PER_EON;
            //
            // // make sure to account for the lookback parameter
            //
            // // for each new eon we compute the multiplier
            // // this is passed to farmer who solves on the new multiplier (after the lookback)
            // // we then add the multiplier to the block so validation can be correct
            // // but how do we know the multiplier is accurate in block?
            // // ledger/manager also needs acces to the difficulty
            //
            // // where is this checked?
            // // when validating the block
            // // when solving the block challenge

            debug!("Closed an eon, block count is {}", block_count);
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
