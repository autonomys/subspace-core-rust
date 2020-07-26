#![allow(dead_code)]

use super::*;
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use async_std::sync::{channel, Receiver, Sender};
use bytes::buf::BufMutExt;
use bytes::{Bytes, BytesMut};
use futures::join;
use ledger::{Block, FullBlock};
use log::*;
use manager::ProtocolMessage;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::fmt::Display;
use std::io::Write;
use std::net::SocketAddr;
use std::{fmt, mem};

/* Todo
 *
 * Fix all unwrap calls
 * Ensure message size does not exceed 16k
 * Refactor both connection loops into a single function
 * Exchange peers on sync (and ensure peers request works)
 * Handle dropped connections with an event
 * Add another peer to replace the dropped one
 * Handle queued messages to dropped connections (in case connection can not be found)
 * Handle empty block responses, currently that peer will randomly come again soon
 * Hanle errors as results
 * Write tests
 * Filter duplicate message with cache at manager using get_id
 * Handle get peers response with outbound message correctly
 *
*/

pub type NodeID = [u8; 32];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NodeType {
    Gateway,
    Peer,
    Farmer,
}

pub struct Node {
    id: NodeID,
    mode: NodeType,
    addr: SocketAddr,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum Message {
    Ping,
    Pong,
    PeersRequest,
    PeersResponse { contacts: Vec<SocketAddr> },
    BlockRequest { index: u32 },
    BlockResponse { index: u32, block: Option<Block> },
    BlockProposal { full_block: FullBlock },
}

impl Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Ping => "Ping",
                Self::Pong => "Pong",
                Self::PeersRequest => "PeersRequest",
                Self::PeersResponse { .. } => "PeersResponse",
                Self::BlockRequest { .. } => "BlockRequest",
                Self::BlockResponse { .. } => "BlockResponse",
                Self::BlockProposal { .. } => "BlockProposal",
            }
        )
    }
}

impl Message {
    pub fn to_bytes(&self) -> Bytes {
        let mut writer = BytesMut::new().writer();
        bincode::serialize_into(&mut writer, self).unwrap();
        writer.into_inner().freeze()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            debug!("Failed to deserialize network message: {}", error);
            ()
        })
    }

    pub fn get_id(&self) -> [u8; 32] {
        crypto::digest_sha_256(&self.to_bytes())
    }
}

enum NetworkEvent {
    NewPeer {
        peer_addr: SocketAddr,
        stream: TcpStream,
    },
    RemovedPeer {
        peer_addr: SocketAddr,
    },
    InboundMessage {
        peer_addr: SocketAddr,
        message: Message,
    },
    OutboundMessage {
        message: ProtocolMessage,
    },
}

pub struct Router {
    node_id: NodeID,
    connections: HashMap<SocketAddr, Sender<Bytes>>,
    peers: HashSet<SocketAddr>,
}

impl Router {
    /// create a new empty router
    pub fn new(node_id: NodeID) -> Router {
        Router {
            node_id,
            connections: HashMap::new(),
            peers: HashSet::new(),
        }
    }

    /// add a new connection, possibly add a new peer
    pub fn add(&mut self, node_addr: SocketAddr, sender: Sender<Bytes>) {
        self.connections.insert(node_addr, sender);

        // if peers is low, add to peers
        // later explicitly ask to reduce churn
        if self.peers.len() < MAX_PEERS {
            self.peers.insert(node_addr);
        }
    }

    /// get a connection by node id
    pub fn get_connection(&self, node_addr: &SocketAddr) -> &Sender<Bytes> {
        self.connections.get(node_addr).unwrap()
    }

    /// remove a connection and peer if connection is removed
    pub fn remove(&mut self, peer_addr: SocketAddr) {
        // ToDo: Add another peer to replace the removed one

        if self.connections.contains_key(&peer_addr) {
            self.connections.remove(&peer_addr);
            self.peers.remove(&peer_addr);
        }
    }

    /// send a message to all peers
    pub async fn gossip(&self, message: Message) {
        let bytes = message.to_bytes();
        for node_addr in self.peers.iter() {
            info!("Sending a {} message to {}", message, node_addr);
            self.send_bytes(node_addr, bytes.clone()).await;
        }
    }

    /// send a message to all but one peer (who sent you the message)
    pub async fn regossip(&self, sender: &SocketAddr, message: Message) {
        let bytes = message.to_bytes();
        for node_addr in self.peers.iter() {
            if node_addr != sender {
                info!("Sending a {} message to {}", message, node_addr);
                self.send_bytes(node_addr, bytes.clone()).await;
            }
        }
    }

    /// send a message to specific node by node_id
    pub async fn send(&self, receiver: &SocketAddr, message: Message) {
        info!("Sending a {} message to {}", message, receiver);
        self.send_bytes(receiver, message.to_bytes()).await
    }

    /// send a message to specific node by node_id
    async fn send_bytes(&self, receiver: &SocketAddr, bytes: Bytes) {
        let client_sender = self.get_connection(receiver);
        client_sender.send(bytes).await
    }

    /// get a peer at random
    pub fn get_random_peer(&self) -> Option<SocketAddr> {
        self.peers.iter().choose(&mut rand::thread_rng()).copied()
    }

    /// get a peer at random excluding a specific peer
    pub fn get_random_peer_excluding(&self, node_addr: SocketAddr) -> Option<SocketAddr> {
        self.peers
            .iter()
            .filter(|&peer_addr| !peer_addr.eq(&node_addr))
            .choose(&mut rand::thread_rng())
            .copied()
    }

    // retrieve the socket addr for each peer, except the one asking
    pub fn get_contacts(&self, exception: &SocketAddr) -> Vec<SocketAddr> {
        self.peers
            .iter()
            .filter(|&peer| !peer.eq(&exception))
            .copied()
            .collect()
    }
}

/// Returns Option<(message_bytes, consumed_bytes)>
fn extract_message(input: &[u8]) -> Option<(Result<Message, ()>, usize)> {
    if input.len() <= 2 {
        None
    } else {
        let (message_length_bytes, remainder) = input.split_at(2);
        let message_length = u16::from_le_bytes(message_length_bytes.try_into().unwrap()) as usize;

        if remainder.len() < message_length {
            None
        } else {
            let message = Message::from_bytes(&remainder[..message_length]);

            Some((message, 2 + message_length))
        }
    }
}

fn read_messages(mut stream: TcpStream) -> Receiver<Result<Message, ()>> {
    let (messages_sender, messages_receiver) = channel(10);

    async_std::task::spawn(async move {
        let header_length = 2;
        let max_message_length = 16 * 1024;
        // We support up to 16 kiB message + 2 byte header, so since we may have message across 2
        // read buffers, allocate enough space to contain up to 2 such messages
        let mut buffer = BytesMut::with_capacity((header_length + max_message_length) * 2);
        let mut buffer_contents_bytes = 0;
        buffer.resize(buffer.capacity(), 0);
        // Auxiliary buffer that we will swap with primary on each iteration
        let mut aux_buffer = BytesMut::with_capacity((header_length + max_message_length) * 2);
        aux_buffer.resize(aux_buffer.capacity(), 0);

        // TODO: Handle error?
        while let Ok(read_size) = stream.read(&mut buffer[buffer_contents_bytes..]).await {
            if read_size == 0 {
                // peer disconnected, exit the loop
                break;
            }

            buffer_contents_bytes += read_size;

            // Read as many messages as possible starting from the beginning
            let mut offset = 0;
            while let Some((message, consumed_bytes)) =
                extract_message(&buffer[offset..buffer_contents_bytes])
            {
                messages_sender.send(message).await;
                // Move cursor forward
                offset += consumed_bytes;
            }

            // Copy unprocessed remainder from `buffer` to `aux_buffer`
            aux_buffer
                .as_mut()
                .write_all(&buffer[offset..buffer_contents_bytes])
                .unwrap();
            // Decrease useful contents length by processed amount
            buffer_contents_bytes -= offset;
            // Swap buffers to avoid additional copying
            mem::swap(&mut aux_buffer, &mut buffer);
        }
    });

    messages_receiver
}

async fn connect(peer_addr: SocketAddr, broker_sender: Sender<NetworkEvent>) {
    let stream = TcpStream::connect(peer_addr).await.unwrap();
    on_connected(peer_addr, stream, broker_sender).await;
}

async fn on_connected(
    peer_addr: SocketAddr,
    stream: TcpStream,
    broker_sender: Sender<NetworkEvent>,
) {
    broker_sender
        .send({
            let stream = stream.clone();

            NetworkEvent::NewPeer { peer_addr, stream }
        })
        .await;

    let mut messages_receiver = read_messages(stream);

    while let Some(message) = messages_receiver.next().await {
        if let Ok(message) = message {
            // info!("{:?}", message);
            broker_sender
                .send(NetworkEvent::InboundMessage { peer_addr, message })
                .await;
        }
    }

    broker_sender
        .send(NetworkEvent::RemovedPeer { peer_addr })
        .await;
}

pub async fn run(
    node_type: NodeType,
    node_id: NodeID,
    local_addr: SocketAddr,
    any_to_main_tx: Sender<ProtocolMessage>,
    main_to_net_rx: Receiver<ProtocolMessage>,
) {
    let gateway_addr: std::net::SocketAddr = DEV_GATEWAY_ADDR.parse().unwrap();
    let (broker_sender, mut broker_receiver) = channel::<NetworkEvent>(32);

    // create the tcp listener
    let addr = if matches!(node_type, NodeType::Gateway) {
        gateway_addr
    } else {
        local_addr
    };
    let socket = TcpListener::bind(addr).await.unwrap();

    let mut connections = socket.incoming();
    info!("Network is listening on TCP socket for inbound connections");

    // receives protocol messages from manager
    let protocol_receiver_loop = async {
        info!("Network is listening for protocol messages");
        loop {
            if let Some(message) = main_to_net_rx.recv().await.ok() {
                // forward to broker as protocol message
                broker_sender
                    .send(NetworkEvent::OutboundMessage { message })
                    .await;
            }
        }
    };

    // receives new connection requests over the TCP socket
    let new_connection_loop = async {
        while let Some(stream) = connections.next().await {
            let broker_sender = broker_sender.clone();

            async_std::task::spawn(async move {
                info!("New inbound TCP connection initiated");

                let stream = stream.unwrap();
                let peer_addr = stream.peer_addr().unwrap();
                on_connected(peer_addr, stream, broker_sender).await;
            });
        }
    };

    // receives network messages from peers and protocol messages from manager
    // maintains an async channel between each open socket and sender half
    let broker_loop = async {
        let mut router = Router::new(node_id);

        while let Some(event) = broker_receiver.next().await {
            match event {
                NetworkEvent::InboundMessage { peer_addr, message } => {
                    // messages received over the network from another peer, send to manager or handle internally
                    info!("Received a {} network message from {}", message, peer_addr);

                    // ToDo: (later) implement a cache of last x messages (only if block or tx)

                    match message {
                        Message::Ping => {
                            // send a pong response

                            router.send(&peer_addr, Message::Pong).await;
                        }
                        Message::Pong => {
                            // do nothing for now

                            // ToDo: latency timing
                        }
                        Message::PeersRequest => {
                            // retrieve peers and send over the wire

                            // ToDo: fully implement and test

                            let contacts = router.get_contacts(&peer_addr);

                            router
                                .send(&peer_addr, Message::PeersResponse { contacts })
                                .await;
                        }
                        Message::PeersResponse { contacts } => {
                            // ToDo: match responses to request id, else ignore

                            // convert binary to peers, for each peer, attempt to connect
                            // need to write another method to add peer on connection
                            for potential_peer_addr in contacts.iter() {
                                let potential_peer = potential_peer_addr.clone();
                                while router.peers.len() < MAX_PEERS {
                                    let broker_sender = broker_sender.clone();
                                    async_std::task::spawn(async move {
                                        connect(potential_peer, broker_sender).await;
                                    });
                                }
                            }

                            // if we still have too few peers, should we try another peer
                        }
                        Message::BlockRequest { index } => {
                            any_to_main_tx
                                .send(ProtocolMessage::BlockRequestFrom(peer_addr, index))
                                .await;
                        }
                        Message::BlockResponse { index, block } => {
                            // if no block in response, request from a different peer
                            match block {
                                Some(block) => {
                                    any_to_main_tx
                                        .send(ProtocolMessage::BlockResponse(block))
                                        .await;
                                }
                                None => {
                                    info!("Peer did not have block at desired index, requesting from a different peer");

                                    let request = Message::BlockRequest { index };

                                    if let Some(new_peer) =
                                        router.get_random_peer_excluding(peer_addr)
                                    {
                                        router.send(&new_peer, request).await;
                                    } else {
                                        info!("Failed to request block: no other peers found");
                                    }
                                    continue;
                                }
                            }
                        }
                        Message::BlockProposal { full_block } => {
                            // send to main

                            any_to_main_tx
                                .send(ProtocolMessage::BlockProposalRemote(full_block, peer_addr))
                                .await;
                        }
                    }
                }
                NetworkEvent::OutboundMessage { message } => {
                    // messages received from manager that need to be sent over the network to peers
                    match message {
                        ProtocolMessage::BlockRequest(index) => {
                            // ledger requested a block at a given index
                            // send a block_request to one peer chosen at random from gossip group

                            if let Some(peer) = router.get_random_peer() {
                                router.send(&peer, Message::BlockRequest { index }).await;
                            } else {
                                info!("Failed to request block at index {}: no peers", index);
                            }
                        }
                        ProtocolMessage::BlockResponseTo(node_addr, block, index) => {
                            // send a block back to a peer that has requested it from you

                            router
                                .send(&node_addr, Message::BlockResponse { index, block })
                                .await;
                        }
                        ProtocolMessage::BlockProposalRemote(full_block, sender_addr) => {
                            // propagating a block received over the network that was valid
                            // do not send back to the node who sent to you

                            router
                                .regossip(&sender_addr, Message::BlockProposal { full_block })
                                .await;
                        }
                        ProtocolMessage::BlockProposalLocal(full_block) => {
                            // propagating a block generated locally, send to all

                            router.gossip(Message::BlockProposal { full_block }).await;
                        }
                        _ => panic!(
                            "Network protocol listener has received an unknown protocol message!"
                        ),
                    }
                }
                NetworkEvent::NewPeer {
                    peer_addr,
                    mut stream,
                } => {
                    info!("Broker is adding a new peer");
                    let (client_sender, mut client_receiver) = channel::<Bytes>(32);
                    router.add(peer_addr, client_sender);

                    // listen for new messages from the broker and send back to peer over stream
                    async_std::task::spawn(async move {
                        while let Some(bytes) = client_receiver.next().await {
                            let length = bytes.len() as u16;
                            stream.write_all(&length.to_le_bytes()).await.unwrap();
                            stream.write_all(&bytes).await.unwrap();
                        }
                    });
                }
                NetworkEvent::RemovedPeer { peer_addr } => {
                    router.remove(peer_addr);
                    info!("Broker has dropped a peer who disconnected");
                }
            }
        }
    };

    let network_startup = async {
        // if not gateway, connect to the gateway
        if node_type != NodeType::Gateway {
            info!("Connecting to gateway node");

            let broker_sender = broker_sender.clone();
            connect(gateway_addr, broker_sender).await;
        }
    };

    join!(
        protocol_receiver_loop,
        new_connection_loop,
        broker_loop,
        network_startup
    );
}
