#![allow(dead_code)]

use super::*;
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use async_std::sync::{channel, Receiver, Sender};
use bytes::{Buf, Bytes, BytesMut};
use futures::join;
use ledger::{Block, FullBlock};
use manager::ProtocolMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryInto;
use std::net::SocketAddr;
use std::sync::Arc;

/* Todo
 *
 * Refactor both connection loops into a single function
 * Exchange peers on sync (and ensure peers request works)
 * Handle dropped connections with an event
 * Handle queued messages to dropped connections (in case connection can not be found)
 * Handle empty block responses, currently that peer will randomly come again soon
 * Hanle errors as results
 * Write tests
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

    pub fn from_bytes(bytes: &[u8]) -> NetworkMessage {
        bincode::deserialize(bytes).unwrap()
    }

    pub fn get_id(&self) -> [u8; 32] {
        crypto::digest_sha_256(&self.to_bytes())
    }
}

enum NetworkEvent {
    NewPeer {
        peer_addr: SocketAddr,
        stream: Arc<TcpStream>,
    },
    DroppedPeer {
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
    peers: Vec<SocketAddr>,
}

impl Router {
    /// create a new empty router
    pub fn new(node_id: NodeID) -> Router {
        Router {
            node_id,
            connections: HashMap::new(),
            peers: Vec::new(),
        }
    }

    /// add a new connection, possibly add a new peer
    pub fn add(&mut self, node_addr: SocketAddr, sender: Sender<NetworkMessage>) {
        self.connections.insert(node_addr, sender);

        // if peers is low, add to peers
        // later explicitly ask to reduce churn
        if self.peers.len() < MAX_PEERS {
            self.peers.push(node_addr);
        }
    }

    /// get a connection by node id
    pub fn get_connection(&self, node_addr: &SocketAddr) -> &Sender<NetworkMessage> {
        self.connections.get(node_addr).unwrap()
    }

    /// remove a connection and peer if connection is dropped
    pub fn drop(&mut self, peer_addr: SocketAddr) {
        // ToDo: Add another peer to replace the dropped one

        if self.connections.contains_key(&peer_addr) {
            self.connections.remove(&peer_addr);
            if self.peers.contains(&peer_addr) {
                let index = self.peers.iter().position(|x| *x == peer_addr).unwrap();
                self.peers.remove(index);
            }
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
        println!("Sending a {:?} message to {:?}", message.name, receiver);
        client_sender.send(message).await
    }

    /// get a peer at random
    pub fn get_random_peer(&self) -> SocketAddr {
        let peer_count = self.peers.len();
        let peer_index = rand::random::<usize>() % peer_count;
        self.peers[peer_index]
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

/// Returns (Option(message_bytes), remaining_buffer)
fn extract_message(mut input: BytesMut) -> (Option<Bytes>, BytesMut) {
    if input.len() <= 2 {
        (None, input)
    } else {
        let message_length = u16::from_le_bytes(input[..2].try_into().unwrap()) as usize;

        if input[2..].len() < message_length {
            (None, input)
        } else {
            let mut input = input.split_off(2);
            let message_bytes = input.split_to(message_length).to_bytes();
            let remaining_buffer = input;

            (Some(message_bytes), remaining_buffer)
        }
    }
}

async fn connect(peer_addr: SocketAddr, broker_sender: Sender<NetworkEvent>) {
    let stream = TcpStream::connect(peer_addr).await.unwrap();

    let mut last_buffer = BytesMut::new();
    let mut current_buffer = BytesMut::with_capacity(16 * 1024 + 2);
    current_buffer.resize(current_buffer.capacity(), 0);

    let stream = Arc::new(stream);
    let cloned_stream = Arc::clone(&stream);
    let mut stream = &*stream;

    broker_sender
        .send(NetworkEvent::NewPeer {
            peer_addr,
            stream: cloned_stream,
        })
        .await;

    while let Ok(read_size) = stream.read(&mut current_buffer).await {
        if read_size == 0 {
            // peer disconnected, exit the loop
            broker_sender
                .send(NetworkEvent::DroppedPeer { peer_addr })
                .await;
            break;
        }

        let mut buffer = BytesMut::from(&current_buffer[..read_size]);
        if !last_buffer.is_empty() {
            buffer.extend_from_slice(&last_buffer);
        }

        last_buffer = loop {
            match extract_message(buffer) {
                (Some(message), remaining_buffer) => {
                    buffer = remaining_buffer;
                    let message = NetworkMessage::from_bytes(&message);
                    // println!("{:?}", message);
                    broker_sender
                        .send(NetworkEvent::InboundMessage { peer_addr, message })
                        .await;
                }
                (None, remaining_buffer) => {
                    break remaining_buffer;
                }
            }
        }
    }
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
    println!("Network is listening on TCP socket for inbound connections");

    // receives protocol messages from manager
    let protocol_receiver_loop = async {
        println!("Network is listening for protocol messages");
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
            let broker_sender_clone = broker_sender.clone();

            async_std::task::spawn(async move {
                println!("New inbound TCP connection initiated");

                let mut last_buffer = BytesMut::new();
                let mut current_buffer = BytesMut::with_capacity(16 * 1024 + 2);
                current_buffer.resize(current_buffer.capacity(), 0);

                let stream = stream.unwrap();
                let peer_addr = stream.peer_addr().unwrap();

                let stream = Arc::new(stream);
                let cloned_stream = Arc::clone(&stream);
                let mut stream = &*stream;

                // notify the broker loop of the peer, passing them the send half
                broker_sender_clone
                    .send(NetworkEvent::NewPeer {
                        peer_addr,
                        stream: cloned_stream,
                    })
                    .await;

                while let Ok(read_size) = stream.read(&mut current_buffer).await {
                    if read_size == 0 {
                        // peer disconnected, exit the loop
                        broker_sender_clone
                            .send(NetworkEvent::DroppedPeer { peer_addr })
                            .await;
                        break;
                    }

                    let mut buffer = BytesMut::from(&current_buffer[..read_size]);
                    if !last_buffer.is_empty() {
                        buffer.extend_from_slice(&last_buffer);
                    }

                    last_buffer = loop {
                        match extract_message(buffer) {
                            (Some(message), remaining_buffer) => {
                                buffer = remaining_buffer;
                                let message = NetworkMessage::from_bytes(&message);
                                // println!("{:?}", message);
                                broker_sender_clone
                                    .send(NetworkEvent::InboundMessage { peer_addr, message })
                                    .await;
                            }
                            (None, remaining_buffer) => {
                                break remaining_buffer;
                            }
                        }
                    }
                }
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
                    println!(
                        "Received a {:?} network message from {:?}",
                        message.name, peer_addr
                    );
                    match message.name {
                        NetworkMessageName::Ping => {
                            // send a pong response
                            router.reply(&peer_addr, message, Vec::new()).await;
                        }
                        NetworkMessageName::Pong => {
                            // do nothing for now
                        }
                        NetworkMessageName::PeersRequest => {
                            // retrieve peers and send over the wire

                            let contacts = router.get_contacts(&peer_addr);

                            let response = NetworkMessage {
                                name: NetworkMessageName::PeersRequest,
                                data: contacts.to_bytes(),
                            };

                            router.send(&peer_addr, response).await;
                        }
                        NetworkMessageName::PeersResponse => {
                            // convert binary to peers, for each peer, attempt to connect
                            // need to write another method to add peer on connection
                            let potential_peers = AddrList::from_bytes(&message.data);
                            for potential_peer_addr in potential_peers.addrs.iter() {
                                let potential_peer = potential_peer_addr.clone();
                                while router.peers.len() < MAX_PEERS {
                                    let broker_sender_clone = broker_sender.clone();
                                    async_std::task::spawn(async move {
                                        connect(potential_peer, broker_sender_clone).await;
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
                                println!("Peer did not have block at desired index, requesting from a different peer");

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
                        _ => {
                            (panic!(
                            "Network protocol listener has received an unknown protocol message!"
                        ))
                        }
                    }
                }
                NetworkEvent::NewPeer { peer_addr, stream } => {
                    println!("Broker is adding a new peer");
                    let (client_sender, mut client_receiver) = channel::<NetworkMessage>(32);
                    router.add(peer_addr, client_sender);

                    // listen for new messages from the broker and send back to peer over stream
                    async_std::task::spawn(async move {
                        let mut stream = &*stream;
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
                NetworkEvent::DroppedPeer { peer_addr } => {
                    router.drop(peer_addr);
                    println!("Broker has dropped a peer who disconnected");
                }
            }
        }
    };

    let network_startup = async {
        // if not gateway, connect to the gateway
        if mode != NodeType::Gateway {
            println!("Connecting to gateway node");

            let broker_sender_clone = broker_sender.clone();
            async_std::task::spawn(async move {
                connect(gateway_addr, broker_sender_clone).await;
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
