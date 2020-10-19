use crate::block::Block;
use crate::{BlockId, ContentId, ProofId};
use log::*;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct MetaBlock {
    pub block: Block,
    pub block_id: BlockId,
    pub proof_id: ProofId,
    pub content_id: ContentId,
    pub children: Vec<ProofId>,
    pub height: u64,
}

pub struct MetaBlocks {
    blocks: HashMap<ProofId, MetaBlock>,
    content_to_proof_map: HashMap<ContentId, ProofId>,
    genesis_challenge: [u8; 32],
}

impl MetaBlocks {
    pub fn new(genesis_challenge: [u8; 32]) -> Self {
        MetaBlocks {
            blocks: HashMap::new(),
            content_to_proof_map: HashMap::new(),
            genesis_challenge,
        }
    }

    pub fn contains_content_id(&self, content_id: &ContentId) -> bool {
        self.content_to_proof_map.contains_key(content_id)
    }

    pub fn get_proof_id_from_content_id(&self, content_id: &ContentId) -> ProofId {
        self.content_to_proof_map
            .get(content_id)
            .expect("Should have content for a valid block")
            .clone()
    }

    pub fn get_metablock_from_content_id(&self, content_id: &ContentId) -> MetaBlock {
        let proof_id = self.get_proof_id_from_content_id(content_id);
        let metablock = self
            .blocks
            .get(&proof_id)
            .expect("In content to proof map")
            .clone();
        metablock
    }

    pub fn get_metablock_from_content_id_as_option(
        &self,
        content_id: &ContentId,
    ) -> Option<MetaBlock> {
        match self.content_to_proof_map.get(content_id) {
            Some(proof_id) => match self.blocks.get(proof_id) {
                Some(metablock) => return Some(metablock.clone()),
                None => return None,
            },
            None => return None,
        }
    }

    pub fn get_metablock_from_proof_id(&self, proof_id: &ProofId) -> MetaBlock {
        self.blocks
            .get(proof_id)
            .expect("In content to proof map")
            .clone()
    }

    pub fn get_metablock_from_proof_id_as_option(&self, proof_id: &ProofId) -> Option<MetaBlock> {
        match self.blocks.get(proof_id) {
            Some(metablock) => Some(metablock.clone()),
            None => None,
        }
    }

    /// Stage a new block received via gossip or created locally
    pub fn save(&mut self, block: Block) -> MetaBlock {
        let block_id = block.get_id();
        let proof_id = block.proof.get_id();
        let content_id = block.content.get_id();
        let mut height = 0;

        // only for proposer blocks
        // skip the genesis block
        if block.content.parent_id.is_some()
            && block.content.parent_id.unwrap() != self.genesis_challenge
        {
            // TODO: handle errors in case we cannot find the parent, for now check in stage block

            // have to get the parent proof id from the content id
            // should be able to switch from seen to unseen at this point

            let parent_proof_id =
                self.get_proof_id_from_content_id(&block.content.parent_id.expect("Checked above"));
            let parent_metablock = self.blocks.get_mut(&parent_proof_id).unwrap();
            parent_metablock.children.push(proof_id);
            height += parent_metablock.height + 1;
        }

        let metablock = MetaBlock {
            block,
            block_id,
            proof_id,
            content_id,
            children: Vec::new(),
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

        self.blocks.insert(proof_id, metablock.clone());
        self.content_to_proof_map.insert(content_id, proof_id);
        debug!(
            "Staged block with content_id: {} and proof_id: {}",
            hex::encode(&content_id[0..8]),
            hex::encode(&proof_id[0..8])
        );

        metablock
    }

    /// Removes a metablock from blocks and content to proof map, returning any child content ids.
    pub fn remove(&mut self, proof_id: &ProofId) -> MetaBlock {
        let removed_metablock = self.blocks.remove(proof_id).expect("Will exist");
        let removed_pointer = self
            .content_to_proof_map
            .remove(&removed_metablock.content_id);
        if removed_pointer.is_none() {
            panic!("Pointer for removed block should exist!");
        }

        warn!(
            "Removed metablock with content_id: {}",
            hex::encode(&removed_metablock.content_id[0..8])
        );

        removed_metablock
    }
}
