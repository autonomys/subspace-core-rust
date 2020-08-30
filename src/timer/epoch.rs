use crate::crypto;
use crate::utils;
use crate::BlockId;
use crate::EpochChallenge;
use crate::SlotChallenge;
use crate::TIMESLOTS_PER_EPOCH;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Epoch {
    /// has the randomness been derived and the epoch closed?
    pub is_closed: bool,
    /// slot indices and vec of block ids, some will be empty, some one, some many
    slots: HashMap<u64, Vec<BlockId>>,
    /// challenges derived from randomness at closure, one per slot
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
            slots: HashMap::new(),
            challenges: Vec::new(),
            randomness,
        }
    }

    /// Returns `true` in case no blocks for this timeslot existed before
    pub(super) fn add_block_to_timeslot(&mut self, timeslot: u64, block_id: BlockId) -> bool {
        let mut new_timeslot = true;
        self.slots
            .entry(timeslot)
            .and_modify(|list| {
                list.push(block_id);
                new_timeslot = false;
            })
            .or_insert(vec![block_id]);

        new_timeslot
    }

    pub fn get_challenge_for_timeslot(&self, timeslot: usize) -> SlotChallenge {
        // TODO: No guarantee index exists
        self.challenges[timeslot]
    }

    pub(super) fn close(&mut self) {
        let xor_result =
            self.slots
                .values()
                .flatten()
                .fold([0u8; 32], |mut randomness, block_id| {
                    utils::xor_bytes(&mut randomness, &block_id[..]);
                    randomness
                });
        self.randomness = crypto::digest_sha_256(&xor_result);

        for timeslot in 0..TIMESLOTS_PER_EPOCH {
            let slot_seed = [&self.randomness[..], &timeslot.to_le_bytes()[..]].concat();
            self.challenges.push(crypto::digest_sha_256(&slot_seed));
        }

        self.is_closed = true;
    }
}
