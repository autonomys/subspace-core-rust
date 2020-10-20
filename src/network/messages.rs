use crate::block::Block;
use crate::manager::GenesisConfig;
use crate::state::{
    BlockHeight, NetworkPieceBundleById, NetworkPieceBundleByIndex, StateBlock, StateBlockId,
};
use crate::transaction::{SimpleCreditTx, Transaction, TxId};
use crate::{ContentId, PieceId, PieceIndex, ProofId};
use bytes::buf::BufMutExt;
use bytes::{Bytes, BytesMut};
use log::*;
use serde::{Deserialize, Serialize};
use static_assertions::_core::fmt;
use static_assertions::_core::fmt::{Debug, Display};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum GossipMessage {
    BlockProposal { block: Block },
    TxProposal { tx: SimpleCreditTx },
}

impl Display for GossipMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::BlockProposal { .. } => "BlockProposal",
                Self::TxProposal { .. } => "TxProposal",
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlocksRequest {
    pub(crate) timeslot: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlocksResponse {
    pub(crate) blocks: Vec<Block>,
    pub(crate) transactions: Vec<SimpleCreditTx>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlockRequestByContentId {
    pub(crate) id: ContentId,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlockResponseByContentId {
    pub(crate) block: Option<Block>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlockRequestByProofId {
    pub(crate) id: ProofId,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlockResponseByProofId {
    pub(crate) block: Option<Block>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TxRequestById {
    pub(crate) id: TxId,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TxResponseById {
    pub(crate) transaction: Option<Transaction>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PieceRequestById {
    pub(crate) id: PieceId,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PieceResponseById {
    pub(crate) piece_bundle: Option<NetworkPieceBundleById>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PieceRequestByIndex {
    pub(crate) index: PieceIndex,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PieceResponseByIndex {
    pub(crate) piece_bundle: Option<NetworkPieceBundleByIndex>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StateBlockRequestById {
    pub(crate) id: StateBlockId,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StateBlockResponseById {
    pub(crate) state_block: Option<StateBlock>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StateBlockRequestByHeight {
    pub(crate) height: BlockHeight,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StateBlockResponseByHeight {
    pub(crate) state_block: Option<StateBlock>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GenesisConfigRequest {}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GenesisConfigResponse {
    pub(crate) genesis_config: GenesisConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum RequestMessage {
    Blocks(BlocksRequest),
    BlockByContentId(BlockRequestByContentId),
    BlockByProofId(BlockRequestByProofId),
    TransactionById(TxRequestById),
    PieceById(PieceRequestById),
    PieceByIndex(PieceRequestByIndex),
    StateById(StateBlockRequestById),
    StateByHeight(StateBlockRequestByHeight),
    GenesisConfig(GenesisConfigRequest),
}

impl Display for RequestMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Blocks { .. } => "Blocks",
                Self::BlockByContentId { .. } => "BlockByContentId",
                Self::BlockByProofId { .. } => "BlockByProofId",
                Self::TransactionById { .. } => "Transaction",
                Self::PieceById { .. } => "PieceById",
                Self::PieceByIndex { .. } => "PieceByIndex",
                Self::StateById { .. } => "StateById",
                Self::StateByHeight { .. } => "StateByHeight",
                Self::GenesisConfig { .. } => "GenesisConfig",
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ResponseMessage {
    Blocks(BlocksResponse),
    BlockByContentId(BlockResponseByContentId),
    BlockByProofId(BlockResponseByProofId),
    TransactionById(TxResponseById),
    PieceById(PieceResponseById),
    PieceByIndex(PieceResponseByIndex),
    StateById(StateBlockResponseById),
    StateByHeight(StateBlockResponseByHeight),
    GenesisConfig(GenesisConfigResponse),
}

impl Display for ResponseMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Blocks { .. } => "Blocks",
                Self::BlockByContentId { .. } => "BlockByContentId",
                Self::BlockByProofId { .. } => "BlockByProofId",
                Self::TransactionById { .. } => "Transaction",
                Self::PieceById { .. } => "PieceById",
                Self::PieceByIndex { .. } => "PieceByIndex",
                Self::StateById { .. } => "StateById",
                Self::StateByHeight { .. } => "StateByHeight",
                Self::GenesisConfig { .. } => "GenesisConfig",
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum InternalRequestMessage {
    Contacts,
}

impl Display for InternalRequestMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Contacts { .. } => "Contacts",
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum InternalResponseMessage {
    Contacts(Vec<SocketAddr>),
}

impl Display for InternalResponseMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Contacts { .. } => "Contacts",
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Message {
    Gossip(GossipMessage),
    Request {
        id: u32,
        message: RequestMessage,
    },
    Response {
        id: u32,
        message: ResponseMessage,
    },
    InternalRequest {
        id: u32,
        message: InternalRequestMessage,
    },
    InternalResponse {
        id: u32,
        message: InternalResponseMessage,
    },
}

impl Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gossip(message) => write!(f, "{}", message),
            Self::Request { id, message } => write!(f, "{}:{}", message, id),
            Self::Response { id, message } => write!(f, "{}:{}", message, id),
            Message::InternalRequest { id, message } => write!(f, "{}:{}", message, id),
            Message::InternalResponse { id, message } => write!(f, "{}:{}", message, id),
        }
    }
}

impl Message {
    pub(crate) fn to_bytes(&self) -> Bytes {
        let mut writer = BytesMut::new().writer();
        bincode::serialize_into(&mut writer, self).unwrap();
        writer.into_inner().freeze()
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            debug!("Failed to deserialize network message: {}", error);
        })
    }
}
