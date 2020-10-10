use crate::ledger::BlockHeight;
use crate::EpochChallenge;
use crate::SlotChallenge;
use crate::TIMESLOTS_PER_EPOCH;
use crate::{crypto, ProofId};
use log::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Epoch {
    /// has the randomness been derived and the epoch closed?
    pub is_closed: bool,
    /// Proofs for this epoch, sorted by block height
    block_heights: BTreeMap<BlockHeight, ProofId>,
    /// challenges derived from randomness at closure, one per timeslot
    challenges: Vec<SlotChallenge>,
    /// overall randomness for this epoch
    pub randomness: EpochChallenge,
}

// TODO: Make into an enum for a cleaner implementation, separate into active and closed epoch
impl Epoch {
    pub(super) fn new(index: u64) -> Epoch {
        let randomness = crypto::digest_sha_256(&index.to_le_bytes());

        Epoch {
            is_closed: false,
            block_heights: BTreeMap::new(),
            challenges: Vec::with_capacity(TIMESLOTS_PER_EPOCH as usize),
            randomness,
        }
    }

    pub(super) fn add_block_at_height(&mut self, block_height: u64, proof_id: ProofId) {
        // TODO: this is being invoked frequently, fix the parameters
        if self.is_closed {
            warn!(
                "Epoch already closed, skipping adding block at height {}",
                block_height
            );
            return;
        }
        debug!(
            "Adding confirmed block to epoch_tracker at block height {}",
            block_height
        );
        self.block_heights
            .entry(block_height)
            .and_modify(|_| {
                panic!(
                    "Two confirmed blocks in epoch tracker at block height: {}",
                    block_height
                )
            })
            .or_insert(proof_id);
    }

    pub fn get_challenge_for_timeslot(&self, timeslot: u64) -> SlotChallenge {
        // TODO: this should panic if the epoch is still open
        let timeslot_index = timeslot % TIMESLOTS_PER_EPOCH;
        // TODO: No guarantee index exists
        self.challenges[timeslot_index as usize]
    }

    pub(super) fn close(&mut self, current_epoch: u64) {
        let randomness: [u8; 32] = match self.block_heights.first_key_value() {
            Some((_, proof_id)) => *proof_id,
            None => {
                warn!("Unable to derive randomness for epoch, no blocks have been confirmed");
                crypto::digest_sha_256(&current_epoch.to_le_bytes()[..])
            }
        };

        self.randomness = crypto::digest_sha_256(&randomness);

        for timeslot_index in 0..TIMESLOTS_PER_EPOCH {
            let slot_seed = [&self.randomness[..], &timeslot_index.to_le_bytes()[..]].concat();
            self.challenges.push(crypto::digest_sha_256(&slot_seed));
        }

        self.is_closed = true;
    }
}
