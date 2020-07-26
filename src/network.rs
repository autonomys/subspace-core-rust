#![allow(dead_code)]

use super::*;
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use async_std::sync::{channel, Receiver, Sender};
use bytes::BytesMut;
use futures::join;
use ledger::{Block, FullBlock};
use log::*;
use manager::ProtocolMessage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io::Write;
use std::mem;
use std::net::SocketAddr;

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
pub enum NetworkMessageName {
    Ping,
    Pong,
    PeersRequest,
    PeersResponse,
    BlockRequest,
    BlockResponse,
    BlockProposal,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct NetworkMessage {
    name: NetworkMessageName,
    data: Vec<u8>,
}

impl NetworkMessage {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<NetworkMessage, ()> {
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
        message: NetworkMessage,
    },
    OutboundMessage {
        message: ProtocolMessage,
    },
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct AddrList {
    addrs: Vec<SocketAddr>,
}

impl AddrList {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> AddrList {
        bincode::deserialize(bytes).unwrap()
    }
}

pub struct Router {
    node_id: NodeID,
    connections: HashMap<SocketAddr, Sender<NetworkMessage>>,
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
    pub fn add(&mut self, node_addr: SocketAddr, sender: Sender<NetworkMessage>) {
        self.connections.insert(node_addr, sender);

        // if peers is low, add to peers
        // later explicitly ask to reduce churn
        if self.peers.len() < MAX_PEERS {
            self.peers.insert(node_addr);
        }
    }

    /// get a connection by node id
    pub fn get_connection(&self, node_addr: &SocketAddr) -> &Sender<NetworkMessage> {
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
    pub async fn gossip(&self, message: NetworkMessage) {
        for node_addr in self.peers.iter() {
            self.send(&node_addr, message.clone()).await;
        }
    }

    /// send a message to all but one peer (who sent you the message)
    pub async fn regossip(&self, sender: &SocketAddr, message: NetworkMessage) {
        for node_addr in self.peers.iter() {
            if node_addr != sender {
                self.send(&node_addr, message.clone()).await;
            }
        }
    }

    /// send a message to specific node by node_id
    pub async fn send(&self, receiver: &SocketAddr, message: NetworkMessage) {
        let client_sender = self.get_connection(receiver);
        info!("Sending a {:?} message to {:?}", message.name, receiver);
        client_sender.send(message).await
    }

    /// get a peer at random
    pub fn get_random_peer(&self) -> SocketAddr {
        let peer_count = self.peers.len();
        let peer_index = rand::random::<usize>() % peer_count;
        *self.peers.iter().skip(peer_index).next().unwrap()
    }

    /// get a peer at random exluding a specific peer
    pub fn get_random_peer_excluding(&self, node_addr: SocketAddr) -> SocketAddr {
        let mut peer_addr = self.get_random_peer();

        while peer_addr == node_addr {
            peer_addr = self.get_random_peer();
        }

        peer_addr
    }

    // retrieve the socket addr for each peer, except the one asking
    pub fn get_contacts(&self, exception: &SocketAddr) -> AddrList {
        let mut contacts: Vec<SocketAddr> = Vec::new();
        for peer_addr in self.peers.iter() {
            if peer_addr != exception {
                contacts.push(*peer_addr);
            }
        }

        AddrList { addrs: contacts }
    }

    /// send an rpc response back to the node that sent you an rpc request
    pub async fn reply(&self, recipient: &SocketAddr, message: NetworkMessage, data: Vec<u8>) {
        let name: NetworkMessageName = match message.name {
            NetworkMessageName::Ping => NetworkMessageName::Pong,
            NetworkMessageName::PeersRequest => NetworkMessageName::PeersResponse,
            NetworkMessageName::BlockRequest => NetworkMessageName::BlockResponse,
            _ => panic!("Network is trying to reply to a non-rpc message type"),
        };

        let response = NetworkMessage { name, data };

        self.send(recipient, response).await;
    }
}

/// Returns Option<(message_bytes, consumed_bytes)>
fn extract_message(input: &[u8]) -> Option<(Result<NetworkMessage, ()>, usize)> {
    if input.len() <= 2 {
        None
    } else {
        let (message_length_bytes, remainder) = input.split_at(2);
        let message_length = u16::from_le_bytes(message_length_bytes.try_into().unwrap()) as usize;

        if remainder.len() < message_length {
            None
        } else {
            let message = NetworkMessage::from_bytes(&remainder[..message_length]);

            Some((message, 2 + message_length))
        }
    }
}

fn read_messages(mut stream: TcpStream) -> Receiver<Result<NetworkMessage, ()>> {
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

            // Copy unprocessed remainder from `buffer` to `tmp_buffer`
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
    mode: NodeType,
    node_id: NodeID,
    local_addr: SocketAddr,
    any_to_main_tx: Sender<ProtocolMessage>,
    main_to_net_rx: Receiver<ProtocolMessage>,
) {
    let gateway_addr: std::net::SocketAddr = DEV_GATEWAY_ADDR.parse().unwrap();
    let (broker_sender, mut broker_receiver) = channel::<NetworkEvent>(32);

    // create the tcp listener
    let socket: TcpListener;
    match mode {
        NodeType::Gateway => {
            socket = TcpListener::bind(gateway_addr).await.unwrap();
        }
        _ => {
            socket = TcpListener::bind(local_addr).await.unwrap();
        }
    }

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
                    info!(
                        "Received a {:?} network message from {:?}",
                        message.name, peer_addr
                    );

                    // ToDo: (later) implement a cache of last x messages (only if block or tx)

                    match message.name {
                        NetworkMessageName::Ping => {
                            // send a pong response

                            // ToDo: fix message type as pong
                            router.reply(&peer_addr, message, Vec::new()).await;
                        }
                        NetworkMessageName::Pong => {
                            // do nothing for now

                            // ToDo: latency timing
                        }
                        NetworkMessageName::PeersRequest => {
                            // retrieve peers and send over the wire

                            // ToDo: fully implement and test

                            let contacts = router.get_contacts(&peer_addr);

                            let response = NetworkMessage {
                                name: NetworkMessageName::PeersResponse,
                                data: contacts.to_bytes(),
                            };

                            router.send(&peer_addr, response).await;
                        }
                        NetworkMessageName::PeersResponse => {
                            // ToDo: match responses to request id, else ignore

                            // convert binary to peers, for each peer, attempt to connect
                            // need to write another method to add peer on connection
                            let potential_peers = AddrList::from_bytes(&message.data);
                            for potential_peer_addr in potential_peers.addrs.iter() {
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
                        NetworkMessageName::BlockRequest => {
                            // parse and send to main
                            let index = utils::bytes_le_to_u32(&message.data[0..4]);
                            any_to_main_tx
                                .send(ProtocolMessage::BlockRequestFrom(peer_addr, index))
                                .await;
                        }
                        NetworkMessageName::BlockResponse => {
                            // if empty block response, request from a different peer
                            if message.data.len() == 4 {
                                info!("Peer did not have block at desired index, requesting from a different peer");

                                let request = NetworkMessage {
                                    name: NetworkMessageName::BlockRequest,
                                    data: message.data[0..4].to_vec(),
                                };

                                let new_peer = router.get_random_peer_excluding(peer_addr);

                                router.send(&new_peer, request).await;
                                continue;
                            }

                            // else forward response as protocol message to main
                            let block = Block::from_bytes(&message.data);
                            any_to_main_tx
                                .send(ProtocolMessage::BlockResponse(block))
                                .await;
                        }
                        NetworkMessageName::BlockProposal => {
                            // parse and send to main

                            let full_block = FullBlock::from_bytes(&message.data);
                            let message =
                                ProtocolMessage::BlockProposalRemote(full_block, peer_addr);
                            any_to_main_tx.send(message).await;
                        }
                    }
                }
                NetworkEvent::OutboundMessage { message } => {
                    // messages received from manager that need to be sent over the network to peers
                    match message {
                        ProtocolMessage::BlockRequest(index) => {
                            // ledger requested a block at a given index
                            // send a block_request to one peer chosen at random from gossip group

                            let peer = router.get_random_peer();

                            let request = NetworkMessage {
                                name: NetworkMessageName::BlockRequest,
                                data: index.to_le_bytes().to_vec(),
                            };

                            router.send(&peer, request).await;
                        }
                        ProtocolMessage::BlockResponseTo(node_addr, block_option, block_index) => {
                            // send a block back to a peer that has requested it from you

                            let data = match block_option {
                                Some(block) => block.to_bytes(),
                                None => block_index.to_le_bytes().to_vec(),
                            };

                            let response = NetworkMessage {
                                name: NetworkMessageName::BlockResponse,
                                data,
                            };

                            router.send(&node_addr, response).await;
                        }
                        ProtocolMessage::BlockProposalRemote(full_block, sender_addr) => {
                            // propagating a block received over the network that was valid
                            // do not send back to the node who sent to you

                            let message = NetworkMessage {
                                name: NetworkMessageName::BlockProposal,
                                data: full_block.to_bytes(),
                            };

                            router.regossip(&sender_addr, message).await;
                        }
                        ProtocolMessage::BlockProposalLocal(full_block) => {
                            // propagating a block generated locally, send to all

                            let message = NetworkMessage {
                                name: NetworkMessageName::BlockProposal,
                                data: full_block.to_bytes(),
                            };

                            router.gossip(message).await;
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
                    let (client_sender, mut client_receiver) = channel::<NetworkMessage>(32);
                    router.add(peer_addr, client_sender);

                    // listen for new messages from the broker and send back to peer over stream
                    async_std::task::spawn(async move {
                        while let Some(message) = client_receiver.next().await {
                            let mut data = message.to_bytes();
                            let len = data.len() as u16;
                            let header = utils::u16_to_bytes_le(len).to_vec();
                            data.insert(0, header[0]);
                            data.insert(1, header[1]);
                            stream.write_all(&data).await.unwrap();
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
        if mode != NodeType::Gateway {
            info!("Connecting to gateway node");

            let broker_sender = broker_sender.clone();
            async_std::task::spawn(async move {
                connect(gateway_addr, broker_sender).await;
            });
        }
    };

    join!(
        protocol_receiver_loop,
        new_connection_loop,
        broker_loop,
        network_startup
    );
}
