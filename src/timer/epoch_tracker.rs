use crate::timer::Epoch;
use crate::CHALLENGE_LOOKBACK;
use async_std::sync::Mutex;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct EpochTracker(Arc<Mutex<HashMap<u64, Epoch>>>);

impl EpochTracker {
    pub async fn get_epoch(&self, epoch_index: u64) -> Epoch {
        self.0.lock().await.get(&epoch_index).unwrap().clone()
    }

    pub async fn get_loopback_epoch(&self, epoch_index: u64) -> Epoch {
        self.get_epoch(epoch_index - CHALLENGE_LOOKBACK).await
    }

    pub async fn create_epoch(&self, epoch_index: u64) {
        self.0
            .lock()
            .await
            .insert(epoch_index, Epoch::new(epoch_index));
    }

    pub async fn close_epoch(&self, epoch_index: u64) {
        self.0
            .lock()
            .await
            .entry(epoch_index)
            .and_modify(|epoch| epoch.close());
    }
}

impl Deref for EpochTracker {
    type Target = Arc<Mutex<HashMap<u64, Epoch>>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
