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
    block_heights: BTreeMap<BlockHeight, Vec<ProofId>>,
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

    /// Returns `true` in case no blocks for this timeslot existed before
    pub(super) fn _add_block_to_timeslot(&mut self, timeslot: u64, block_id: BlockId) {
        if self.is_closed {
            warn!(
                "Epoch already closed, skipping adding block to time slot {}",
                timeslot
            );
            return;
        }
        debug!("Adding block to time slot");
        let timeslot_index = timeslot % TIMESLOTS_PER_EPOCH;
        self.timeslots
            .entry(timeslot_index)
            .and_modify(|list| {
                list.push(block_id);
            })
            .or_insert_with(|| vec![block_id]);
    }

    pub(super) fn add_block_at_height(&mut self, block_height: u64, proof_id: ProofId) {
        if self.is_closed {
            warn!(
                "Epoch already closed, skipping adding block at height {}",
                block_height
            );
            return;
        }
        debug!("Adding block at block height");
        self.block_heights
            .entry(block_height)
            // TODO: should be sorted on entry
            .and_modify(|proof_ids| proof_ids.push(proof_id))
            .or_insert(vec![proof_id]);
    }

    pub fn get_challenge_for_timeslot(&self, timeslot: u64) -> SlotChallenge {
        // TODO: this should panic if the epoch is still open
        let timeslot_index = timeslot % TIMESLOTS_PER_EPOCH;
        // TODO: No guarantee index exists
        self.challenges[timeslot_index as usize]
    }

    pub(super) fn _close(&mut self) {
        let xor_result =
            self.timeslots
                .values()
                .flatten()
                .fold(self.randomness, |mut randomness, block_id| {
                    utils::xor_bytes(&mut randomness, &block_id[..]);
                    randomness
                });
        self.randomness = crypto::digest_sha_256(&xor_result);

        for timeslot_index in 0..TIMESLOTS_PER_EPOCH {
            let slot_seed = [&self.randomness[..], &timeslot_index.to_le_bytes()[..]].concat();
            self.challenges.push(crypto::digest_sha_256(&slot_seed));
        }

        self.is_closed = true;
    }

    pub(super) fn close_new(&mut self, blocks_on_longest_chain: &HashSet<ProofId>) {
        let mut deepest_proof = ProofId::default();

        // TODO: this will fail in the unlikely but possible event that there are no valid blocks in an epoch
        // TODO: think more about ways this could fail with an attack or late blocks and handle
        for proof_id in self.block_heights.first_key_value().unwrap().1.into_iter() {
            if blocks_on_longest_chain.contains(proof_id) {
                deepest_proof = *proof_id;
                break;
            }
            // TODO: add expect if not found
        }

        self.randomness = crypto::digest_sha_256(&deepest_proof);

        for timeslot_index in 0..TIMESLOTS_PER_EPOCH {
            let slot_seed = [&self.randomness[..], &timeslot_index.to_le_bytes()[..]].concat();
            self.challenges.push(crypto::digest_sha_256(&slot_seed));
        }

        self.is_closed = true;
    }
}
