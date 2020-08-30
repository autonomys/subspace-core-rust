use crate::timer::Epoch;
use async_std::sync::Mutex;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct EpochTracker(Arc<Mutex<HashMap<u64, Epoch>>>);

impl Deref for EpochTracker {
    type Target = Arc<Mutex<HashMap<u64, Epoch>>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
