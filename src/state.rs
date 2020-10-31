use crate::manager::ProtocolMessage;
use crate::{crypto, NodeID, Piece, PIECES_PER_STATE_BLOCK, PIECE_SIZE, STATE_BLOCK_SIZE_IN_BYTES};
use async_std::sync::Sender;
use itertools::izip;
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::convert::TryInto;

/* TODO

    IMMEDIATE

    - Add index pieces to state
    - Add erasure coding to state
    - Add remaining fields to state block

   LATER

   - Allow pieces to be added directly to a state block
   - Allow pieces to be padded s.t. they can be parsed individually (instead of parsing the entire state block)

*/

#[derive(PartialEq, Debug, Clone)]
pub struct PieceBundle {
    pub piece: Piece,
    pub piece_id: [u8; 32],
    pub piece_index: u64,
    pub piece_proof: Vec<u8>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPieceBundleByIndex {
    pub encoding: Vec<u8>,
    pub piece_proof: Vec<u8>,
    pub node_id: NodeID,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPieceBundleById {
    pub encoding: Vec<u8>,
    pub piece_proof: Vec<u8>,
    pub piece_index: u64,
}

#[derive(PartialEq, Debug, Clone)]
pub struct StateBundle {
    pub state_block: StateBlock,
    pub merkle_root: MerkleRoot,
    pub piece_bundles: Vec<PieceBundle>,
}

pub type StateBlockId = [u8; 32];
pub type BlockHeight = u64;
pub type MerkleRoot = [u8; 32];

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct StateBlock {
    pub previous_state_block_id: StateBlockId,
    pub piece_merkle_root: MerkleRoot,
    pub height: BlockHeight,
}

impl StateBlock {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            warn!("Failed to deserialize State Block: {}", error);
        })
    }

    pub fn get_id(&self) -> StateBlockId {
        crypto::digest_sha_256(&self.to_bytes())
    }
}

pub struct State {
    pending_state: Vec<u8>,
    blocks_by_id: HashMap<StateBlockId, StateBlock>,
    blocks_by_height: BTreeMap<BlockHeight, StateBlockId>,
    last_state_block_id: StateBlockId,
    last_state_block_height: BlockHeight,
    state_sender: Sender<ProtocolMessage>,
}

impl State {
    pub fn new(state_sender: Sender<ProtocolMessage>) -> State {
        State {
            pending_state: Vec::new(),
            blocks_by_id: HashMap::new(),
            blocks_by_height: BTreeMap::new(),
            last_state_block_id: StateBlockId::default(),
            last_state_block_height: 0,
            state_sender,
        }
    }

    /// Create a fixed number of genesis pieces, state updates, and state blocks from a seed string
    pub async fn create_genesis_state(
        &mut self,
        seed: &str,
        piece_count: usize,
    ) -> Vec<PieceBundle> {
        if piece_count % PIECES_PER_STATE_BLOCK != 0 {
            panic!("Genesis piece set must always create an exact number of state blocks")
        }

        let mut input = seed.as_bytes().to_vec();
        let mut piece_bundles: Vec<PieceBundle> = Vec::new();

        for _ in 0..piece_count {
            input = crypto::digest_sha_256_simple(&input[..]);
            let piece_seed = input[..].try_into().expect("32 bytes");
            let data = crypto::genesis_data_from_seed(piece_seed);
            match self.add_data(data).await {
                Some(state_bundle) => state_bundle
                    .piece_bundles
                    .iter()
                    .for_each(|piece_bundle| piece_bundles.push(piece_bundle.clone())),
                None => {}
            }
        }

        piece_bundles
    }

    /// Apply length prefixed state to buffer, encode state when buffer is full.
    pub async fn add_data(&mut self, mut data: Vec<u8>) -> Option<StateBundle> {
        // TODO: add an assertion
        let data_len = data.len() as u16;
        let mut len_as_bytes = data_len.to_be_bytes().to_vec();
        let full_len = data_len + 2;

        let mut state_bundle_option: Option<StateBundle> = None;

        // if data reaches the boundary exactly: add state -> encode
        // if data passes the boundary: encode -> add state
        // if data does not reach boundary: just add state

        let new_len = self.pending_state.len() + full_len as usize;

        // TODO: maybe rewrite as if/else-if/else

        // add data first
        if new_len == STATE_BLOCK_SIZE_IN_BYTES {
            self.pending_state.append(&mut len_as_bytes);
            self.pending_state.append(&mut data);
        }

        // encode state if boundary is reached
        if new_len >= STATE_BLOCK_SIZE_IN_BYTES {
            // create bundle and send
            let state_bundle = self.encode();
            self.state_sender
                .send(ProtocolMessage::StateBundle {
                    state_bundle: state_bundle.clone(),
                })
                .await;

            state_bundle_option = Some(state_bundle);
        }

        // else add data after
        if new_len != STATE_BLOCK_SIZE_IN_BYTES {
            self.pending_state.append(&mut len_as_bytes);
            self.pending_state.append(&mut data);
        }

        state_bundle_option
    }

    /// Encode new state once the pending state buffer is full
    fn encode(&mut self) -> StateBundle {
        // drain the pending state
        let mut new_state: Vec<u8> = self.pending_state.drain(..).collect::<Vec<u8>>();

        // pad to state block size
        new_state.resize(STATE_BLOCK_SIZE_IN_BYTES, 0u8);

        // TODO: erasure code state here: 1:1 ratio of source:parity pieces

        // slice into pieces
        let pieces: Vec<Piece> = new_state
            .chunks_exact(PIECE_SIZE)
            .map(|piece| piece.try_into().unwrap())
            .collect();

        let piece_ids: Vec<[u8; 32]> = pieces
            .iter()
            .map(|piece| crate::crypto::digest_sha_256(piece))
            .collect();

        // compute merkle root and proofs
        let (merkle_root, merkle_proofs) = crate::crypto::create_merkle_tree(&piece_ids);

        let mut piece_bundles: Vec<PieceBundle> = Vec::new();
        let mut piece_index = PIECES_PER_STATE_BLOCK as u64 * self.last_state_block_height;
        for (piece, piece_id, piece_proof) in izip!(&pieces, piece_ids, merkle_proofs) {
            piece_bundles.push(PieceBundle {
                piece_id,
                piece: *piece,
                piece_proof,
                piece_index,
            });
            piece_index += 1;
        }

        // create the state block
        let state_block = StateBlock {
            previous_state_block_id: self.last_state_block_id,
            piece_merkle_root: merkle_root,
            height: self.last_state_block_height,
        };

        // append to the state chain
        self.save_state_block(&state_block);

        StateBundle {
            state_block: state_block.clone(),
            merkle_root,
            piece_bundles,
        }
    }

    /// Save the state block and append to the state chain
    pub fn save_state_block(&mut self, state_block: &StateBlock) {
        let state_block_id = state_block.get_id();
        self.blocks_by_id
            .insert(state_block_id, state_block.clone());
        self.blocks_by_height
            .insert(state_block.height, state_block_id);

        self.last_state_block_height += 1;

        // TODO: ensure we don't increment by more than one

        self.last_state_block_id = state_block_id;
    }

    pub fn get_state_block_by_height(&self, height: BlockHeight) -> Option<&StateBlock> {
        match self.blocks_by_height.get(&height) {
            Some(id) => return self.get_state_block_by_id(id),
            None => {
                warn!(
                    "No state blocks in blocks by height. {} blocks total",
                    self.blocks_by_height.len()
                );
                None
            }
        }
    }

    pub fn get_state_block_by_id(&self, id: &StateBlockId) -> Option<&StateBlock> {
        self.blocks_by_id.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::digest_sha_256_simple;
    use async_std::sync::channel;
    use merkle_tree_binary::Tree;

    #[async_std::test]
    async fn add_data_to_state() {
        let (tx, _) = channel::<ProtocolMessage>(32);
        let mut state = State::new(tx);

        // add 32 bytes of random data
        let data = crypto::random_bytes_32();
        state.add_data(data.to_vec()).await;

        // 32 byte data plus 8 byte length = 40 bytes of state
        let mut expected_length = 32 + 2;
        assert_eq!(state.pending_state.len(), expected_length);

        // add another 4096 bytes of random data
        let data = crypto::generate_random_piece();
        state.add_data(data.to_vec()).await;

        expected_length += 4096 + 2;
        assert_eq!(state.pending_state.len(), expected_length);
    }

    #[test]
    fn state_block_workflow() {
        let (tx, _) = channel::<ProtocolMessage>(32);
        let mut state = State::new(tx);

        let state_block = StateBlock {
            previous_state_block_id: crypto::random_bytes_32(),
            piece_merkle_root: crypto::random_bytes_32(),
            height: 0,
        };

        let state_block_id = state_block.get_id();

        state.save_state_block(&state_block);

        // can we get the state block by id?
        let state_block_from_id_map = state
            .get_state_block_by_id(&state_block_id)
            .expect("Was just added");

        // can we get the state block by height?
        let state_block_from_height_map = state
            .get_state_block_by_height(state_block.height)
            .expect("Was just added");

        // do they match
        assert_eq!(
            state_block_from_id_map.get_id(),
            state_block_from_height_map.get_id()
        );

        // is the last state block id correct?
        assert_eq!(state.last_state_block_id, state_block_id);

        // is the last height correct?
        assert_eq!(state.last_state_block_height, state_block.height);
    }

    #[async_std::test]
    async fn encode_state() {
        let (tx, _) = channel::<ProtocolMessage>(32);
        let mut state = State::new(tx);

        // add enough data to encode state -> 1,050,624 bytes here (256 x (4096 + 8))
        // state should be encoded every 1,048,576 bytes (1024 x 1024)
        let piece_count = 256;
        let mut bundle: Option<StateBundle> = None;

        for _ in 0..piece_count {
            let data = crypto::generate_random_piece();
            let maybe_bundle = state.add_data(data.to_vec()).await;
            if maybe_bundle.is_some() {
                bundle = maybe_bundle;
            }
        }

        // ensure new state was encoded correctly
        match bundle {
            Some(state_bundle) => {
                // ensure we have correct number of pieces
                assert_eq!(state_bundle.piece_bundles.len(), PIECES_PER_STATE_BLOCK);

                // ensure each merkle proof is correct for merkle root and piece id
                state_bundle.piece_bundles.iter().for_each(|piece_bundle| {
                    assert!(Tree::check_proof(
                        &state_bundle.merkle_root,
                        &piece_bundle.piece_proof,
                        &piece_bundle.piece_id,
                        digest_sha_256_simple
                    ));
                });

                // ensure each piece index is correct
            }
            None => assert!(false),
        }

        // ensure a new state block is created
        assert_eq!(state.blocks_by_id.len(), 1);
    }

    #[async_std::test]
    async fn create_genesis_state() {
        let (tx, _) = channel::<ProtocolMessage>(32);
        let mut state = State::new(tx);
        let piece_count = 256;
        let piece_bundles = state.create_genesis_state("SUBSPACE", piece_count).await;

        // ensure the correct number of state blocks were created
        assert_eq!(
            state.blocks_by_id.len(),
            piece_count / PIECES_PER_STATE_BLOCK
        );

        // ensure there is no remaining pending state
        assert_eq!(state.pending_state.len(), 0);

        // did we get the state bundle for plotting?
        assert!(piece_bundles.len() > 0);

        // ensure we can get the first state block
        state.get_state_block_by_height(0);
    }

    #[test]
    fn parse_state() {
        // create a series of state updates and retain (blocks and txs)
        // apply each update
        // encode state
        // from the piece set, parse out the updates
        // ensure the updates match the data
    }
}
