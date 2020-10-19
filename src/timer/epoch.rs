use crate::ledger::BlockHeight;
use crate::EpochChallenge;
use crate::{crypto, ProofId};
use log::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Epoch {
    /// has the randomness been derived and the epoch closed?
    pub is_closed: bool,
    /// Proofs for this epoch, sorted by block height
    block_heights: BTreeMap<BlockHeight, Vec<ProofId>>,
    /// overall randomness for this epoch
    pub randomness: EpochChallenge,
}

// TODO: Make into an enum for a cleaner implementation, separate into active and closed epoch
impl Epoch {
    pub(super) fn new(index: u64) -> Epoch {
        let initial_randomness = crypto::digest_sha_256(&index.to_le_bytes());

        Epoch {
            is_closed: false,
            block_heights: BTreeMap::new(),
            randomness: initial_randomness,
        }
    }

    pub(super) fn add_block_at_height(&mut self, block_height: u64, proof_id: ProofId) {
        if self.is_closed {
            warn!(
                "Epoch already closed, skipping adding block at height {}",
                block_height
            );
            return;
        }

        debug!(
            "Adding staged proposer block to epoch_tracker at block height {}",
            block_height
        );

        self.block_heights
            .entry(block_height)
            .and_modify(|proof_ids| {
                proof_ids.push(proof_id);
            })
            .or_insert(vec![proof_id]);
    }

    pub(super) fn remove_block_at_height(&mut self, block_height: u64, proof_id: ProofId) {
        if self.is_closed {
            warn!(
                "Epoch already closed, skipping removing block at height {}",
                block_height
            );
            return;
        }

        debug!(
            "Removing pruned block from epoch_tracker at block height {}",
            block_height
        );

        let mut is_last = false;
        self.block_heights
            .entry(block_height)
            .and_modify(|proof_ids| {
                let index = proof_ids.iter().position(|i| *i == proof_id).unwrap();
                proof_ids.remove(index);
                if proof_ids.len() == 0 {
                    is_last = true;
                }
            });

        if is_last {
            self.block_heights.remove(&block_height);
        }
    }

    pub(super) fn close(&mut self, current_epoch: u64) {
        if current_epoch != 0 {
            match self.block_heights.first_key_value() {
                Some((_, proof_ids)) => {
                    if proof_ids.len() > 1 {
                        // proof_ids.iter().for_each(|proof_id| {
                        //     warn!("ProofID: {}", hex::encode(&proof_id[0..8]))
                        // });
                        panic!("Unable to close randomness for epoch, long running fork");
                    } else {
                        self.randomness = crypto::digest_sha_256(&proof_ids[0]);
                    }
                }
                None => {
                    panic!("Unable to close randomness for epoch, no blocks have been added");
                    self.randomness = crypto::digest_sha_256(&current_epoch.to_le_bytes()[..])
                }
            }
        };

        self.block_heights.clear();
        self.is_closed = true;
    }
}
