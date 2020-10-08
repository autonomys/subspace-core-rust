use crate::ledger::BlockHeight;
use crate::utils;
use crate::BlockId;
use crate::EpochChallenge;
use crate::SlotChallenge;
use crate::TIMESLOTS_PER_EPOCH;
use crate::{crypto, ProofId};
use log::*;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct Epoch {
    /// has the randomness been derived and the epoch closed?
    pub is_closed: bool,
    /// timeslot indices and vec of block ids, some will be empty, some one, some many
    timeslots: HashMap<u64, Vec<BlockId>>,
    block_heights: BTreeMap<BlockHeight, ProofId>,
    /// challenges derived from randomness at closure, one per timeslot
    challenges: Vec<SlotChallenge>,
    /// overall randomness for this epoch
    pub randomness: EpochChallenge,
    /// Solution range used for this epoch
    pub solution_range: u64,
}

// TODO: Make into an enum for a cleaner implementation, separate into active and closed epoch
impl Epoch {
    pub(super) fn new(index: u64, solution_range: u64) -> Epoch {
        let randomness = crypto::digest_sha_256(&index.to_le_bytes());

        Epoch {
            is_closed: false,
            timeslots: HashMap::new(),
            block_heights: BTreeMap::new(),
            challenges: Vec::with_capacity(TIMESLOTS_PER_EPOCH as usize),
            randomness,
            solution_range,
        }
    }

    /// Counts the number of blocks present in the epoch once it is closed
    pub(super) fn get_block_count(&self) -> u64 {
        // TODO: change to count from block heights
        self.timeslots.values().map(Vec::len).sum::<usize>() as u64
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
            // TODO: should be sorted on entry
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

    pub(super) fn close(&mut self) {
        let deepest_proof: ProofId = match self.block_heights.first_key_value() {
            Some((_, proof_id)) => *proof_id,
            None => {
                // TOOD: derive from last epoch randomness instead
                error!("Unable to derive randomness for epoch, no blocks have been confirmed");
                [0u8; 32]
            }
        };

        self.randomness = crypto::digest_sha_256(&deepest_proof);

        for timeslot_index in 0..TIMESLOTS_PER_EPOCH {
            let slot_seed = [&self.randomness[..], &timeslot_index.to_le_bytes()[..]].concat();
            self.challenges.push(crypto::digest_sha_256(&slot_seed));
        }

        self.is_closed = true;
    }
}
