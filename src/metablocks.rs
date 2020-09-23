use crate::block::Block;
use crate::{BlockId, ContentId, ProofId};
use log::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BlockStatus {
    /// block is not in the blocks db
    Unknown,
    /// block has been cached during ledger sync (early blocks)
    Cached,
    /// block is pending but has not been referenced by another staged block
    Staged,
    /// block is pending and has been referenced by another staged block
    Referenced,
    /// block (and txs) have been applied to balances
    Applied,
}

#[derive(Debug, Clone)]
pub struct MetaBlock {
    pub block: Block,
    pub status: BlockStatus,
    pub block_id: BlockId,
    pub proof_id: ProofId,
    pub content_id: ContentId,
    pub children: HashSet<ProofId>,
    pub height: u64,
}

pub struct MetaBlocks {
    pub blocks: HashMap<ProofId, MetaBlock>,
    content_to_proof_map: HashMap<ContentId, ProofId>,
}

impl MetaBlocks {
    pub fn new() -> Self {
        MetaBlocks {
            blocks: HashMap::new(),
            content_to_proof_map: HashMap::new(),
        }
    }

    pub fn contains_key(&self, proof_id: &ProofId) -> bool {
        self.blocks.contains_key(proof_id)
    }

    pub fn get_status(&self, proof_id: &ProofId) -> BlockStatus {
        match self.blocks.get(proof_id) {
            Some(metablock) => metablock.status,
            None => BlockStatus::Unknown,
        }
    }

    pub fn get_proof_id_from_content_id(&self, content_id: ContentId) -> ProofId {
        self.content_to_proof_map
            .get(&content_id)
            .expect("Should have content for a valid block")
            .clone()
    }

    /// save a block received via gossip during sync
    pub fn cache(&mut self, block: Block) {
        let block_id = block.get_id();
        let proof_id = block.proof.get_id();
        let content_id = block.content.get_id();

        let metablock = MetaBlock {
            block,
            status: BlockStatus::Cached,
            block_id,
            proof_id,
            content_id,
            height: u64::default(),
            children: HashSet::new(),
        };

        self.blocks.insert(proof_id, metablock);
        self.content_to_proof_map.insert(content_id, proof_id);
    }

    /// Stage a new block received via gossip or created locally
    pub fn stage(&mut self, block: Block) -> MetaBlock {
        let block_id = block.get_id();
        let proof_id = block.proof.get_id();
        let content_id = block.content.get_id();
        let mut height = 0;

        // skip the genesis block
        if block.proof.timeslot != 0 {
            // TODO: handle errors in case we cannot find the parent, for now check in stage block

            // have to get the parent proof id from the content id
            // should be able to switch from seen to unseen at this point

            let parent_proof_id = self.get_proof_id_from_content_id(block.content.parent_id);
            let parent_metablock = self.blocks.get_mut(&parent_proof_id).unwrap();
            parent_metablock.children.insert(proof_id);
            height += parent_metablock.height + 1;
        }

        let metablock = MetaBlock {
            block,
            status: BlockStatus::Staged,
            block_id,
            proof_id,
            content_id,
            children: HashSet::new(),
            height,
        };

        // if we have, check if different block_id (and handle), else insert
        if self.blocks.contains_key(&proof_id) {
            let duplicated_metablock = self.blocks.get(&proof_id).unwrap();
            if duplicated_metablock.block_id != metablock.block_id {
                // TODO: handle fraud proof and burning plot
                panic!("Two contents are being used for the same proof!")
            }
        }

        // if we have a cached block, this will update status to staged

        self.blocks.insert(proof_id, metablock.clone());
        self.content_to_proof_map.insert(content_id, proof_id);
        debug!(
            "Staged block with content_id: {} and proof_id: {}",
            hex::encode(&content_id[0..8]),
            hex::encode(&proof_id[0..8])
        );

        metablock
    }

    /// Changes a block status to referenced, may be called more than once on a block
    pub fn reference(&mut self, proof_id: ProofId) {
        let metablock = self
            .blocks
            .get_mut(&proof_id)
            .expect("Pending block should exist");

        if metablock.status == BlockStatus::Staged || metablock.status == BlockStatus::Referenced {
            metablock.status = BlockStatus::Referenced;
        } else {
            panic!("A referenced block must be in a pending state!");
        }
    }

    /// Changes a block status to applied, may only be called once on a block
    pub fn apply(&mut self, proof_id: ProofId) {
        let metablock = self
            .blocks
            .get_mut(&proof_id)
            .expect("Pending block should exist");

        if metablock.status != BlockStatus::Referenced {
            panic!("An applied block must be in a referenced state");
        }

        metablock.status = BlockStatus::Applied;
    }
}
