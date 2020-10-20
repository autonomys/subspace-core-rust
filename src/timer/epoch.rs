use crate::EpochChallenge;
use crate::{crypto, ProofId};
use log::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Epoch {
    /// has the randomness been derived and the epoch closed?
    pub is_closed: bool,
    /// overall randomness for this epoch
    pub randomness: EpochChallenge,
    /// All possible branches for this epoch
    pub branches: Vec<Vec<ProofId>>,
}

impl Epoch {
    pub(super) fn new(index: u64) -> Epoch {
        let initial_randomness = crypto::digest_sha_256(&index.to_le_bytes());

        Epoch {
            is_closed: false,
            randomness: initial_randomness,
            branches: Vec::new(),
        }
    }

    /// Add a valid proof to a branch
    pub(super) fn add_proof(&mut self, parent_proof_id: ProofId, new_proof_id: ProofId) {
        // expected case on first call for epoch, first entry will always create a new branch
        if self.branches.len() == 0 {
            self.branches.push(vec![new_proof_id]);
            trace!("Creating the first branch for epoch_tracker");
            return;
        }

        for (branch_index, branch) in self.branches.clone().iter().enumerate() {
            for (proof_index, proof_id) in branch.iter().rev().enumerate() {
                if *proof_id == parent_proof_id {
                    // we have found the parent
                    if proof_index == 0 {
                        // expected case on subsequent calls in this epoch, extends tail of a branch
                        self.branches[branch_index].push(new_proof_id);
                        trace!("Extending an existing branch for epoch_tracker");
                        return;
                    }

                    // edge case 1, forks the branch and creates a new branch from subset
                    let mut new_branch = branch.clone();
                    new_branch.truncate(branch.len() - proof_index);
                    new_branch.push(new_proof_id);
                    self.branches.push(new_branch);
                    trace!("Creating a new branch from existing branch for epoch_tracker");
                    return;
                }
            }
        }

        // edge case 2, parent not found, creates a new branch
        self.branches.push(vec![new_proof_id]);
    }

    /// Close the epoch and derive randomness from the proof with most confirmations
    pub(super) fn close(&mut self, current_epoch: u64) {
        let mut branch_indices_by_length: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        self.branches
            .iter()
            .enumerate()
            .for_each(|(index, branch)| {
                branch_indices_by_length
                    .entry(branch.len())
                    .and_modify(|indicies| indicies.push(index))
                    .or_insert(vec![index]);
            });

        self.randomness = match branch_indices_by_length.first_key_value() {
            Some((_, indices)) => {
                if indices.len() == 1 {
                    // single branch with most confirmations, take the first proof
                    self.branches[indices[0]]
                        .first()
                        .expect("Has a proof")
                        .clone()
                } else {
                    // multiple branches with the same length, check to see if they share the same first proof
                    let first_proof_id = self.branches[indices[0]].first().expect("Has a proof");
                    for branch_index in indices.iter().skip(1) {
                        if self.branches[*branch_index].first().expect("Has a proof")
                            != first_proof_id
                        {
                            error!("Epoch cannot be  properly closed due to a deep fork");
                        }
                    }
                    first_proof_id.clone()
                }
            }
            None => {
                // TODO: should this derive base randomness instead?
                error!("Epoch cannot be closed, no new proposer blocks have been found");
                let randomness = crypto::digest_sha_256(&current_epoch.to_le_bytes()[..]);
                randomness
            }
        };

        self.branches.clear();
        self.is_closed = true;
    }
}
