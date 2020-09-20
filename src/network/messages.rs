use crate::block::Block;
use crate::transaction::SimpleCreditTx;
use bytes::buf::BufMutExt;
use bytes::{Bytes, BytesMut};
use log::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use static_assertions::_core::fmt;
use static_assertions::_core::fmt::{Debug, Display};

pub(crate) trait Request: Debug + Serialize {
    type Response: DeserializeOwned;
}

pub(crate) trait ToBytes {
    fn to_bytes(&self) -> Bytes;
}

pub(crate) trait FromBytes {
    fn from_bytes(bytes: &[u8]) -> Result<Self, ()>
    where
        Self: Sized;
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub(crate) enum GossipMessage {
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

impl Request for BlocksRequest {
    type Response = BlocksResponse;
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BlocksResponse {
    pub(crate) blocks: Vec<Block>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum RequestMessage {
    BlocksRequest(BlocksRequest),
}

impl Display for RequestMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::BlocksRequest { .. } => "BlockRequest",
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ResponseMessage {
    BlocksResponse(BlocksResponse),
}

impl Display for ResponseMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::BlocksResponse { .. } => "BlockResponse",
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Message {
    Gossip(GossipMessage),
    Request { id: u32, message: RequestMessage },
    Response { id: u32, message: ResponseMessage },
}

impl Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gossip(message) => write!(f, "{}", message),
            Self::Request { id, message } => write!(f, "{}:{}", message, id),
            Self::Response { id, message } => write!(f, "{}:{}", message, id),
        }
    }
}

impl ToBytes for Message {
    fn to_bytes(&self) -> Bytes {
        let mut writer = BytesMut::new().writer();
        bincode::serialize_into(&mut writer, self).unwrap();
        writer.into_inner().freeze()
    }
}

impl FromBytes for Message {
    fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            debug!("Failed to deserialize network message: {}", error);
        })
    }
}