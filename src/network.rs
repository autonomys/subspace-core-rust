pub(crate) mod messages;

use crate::block::Block;
use crate::network::messages::{InternalRequestMessage, InternalResponseMessage};
use crate::{console, MAX_PEERS};
use async_std::net::{TcpListener, TcpStream};
use async_std::sync::{channel, Receiver, Sender};
use async_std::task::JoinHandle;
use bytes::{Bytes, BytesMut};
use futures::lock::Mutex as AsyncMutex;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use futures_lite::future;
use log::*;
use messages::{BlocksRequest, GossipMessage, Message, RequestMessage, ResponseMessage};
use rand::prelude::*;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::fmt::{Debug, Display};
use std::io::Write;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;
use std::{fmt, io, mem};

/* Todo
 *
 * Fix all unwrap calls
 * Ensure message size does not exceed 16k
 * Exchange peers on sync (and ensure peers request works)
 * Add another peer to replace the dropped one
 * Handle empty block responses, currently that peer will randomly come again soon
 * Handle errors as results
 * Write tests
 * Filter duplicate message with cache at manager using get_id
 * Handle get peers response with outbound message correctly
 *
*/

const MAX_MESSAGE_CONTENTS_LENGTH: usize = 2usize.pow(16) - 1;
// TODO: What should this timeout be?
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

pub type NodeID = [u8; 32];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NodeType {
    Gateway,
    Peer,
    Farmer,
}

impl Display for NodeType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            NodeType::Gateway => write!(f, "Gateway"),
            NodeType::Farmer => write!(f, "Farmer"),
            NodeType::Peer => write!(f, "Peer"),
        }
    }
}

impl FromStr for NodeType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "peer" => Ok(Self::Peer),
            "farmer" => Ok(Self::Farmer),
            "gateway" => Ok(Self::Gateway),
            _ => Err(()),
        }
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
        let max_message_length = MAX_MESSAGE_CONTENTS_LENGTH;
        // We support up to 16 kiB message + 2 byte header, so since we may have message across 2
        // read buffers, allocate enough space to contain up to 2 such messages
        let mut buffer = BytesMut::with_capacity((header_length + max_message_length) * 2);
        let mut buffer_contents_bytes = 0;
        buffer.resize(buffer.capacity(), 0);
        // Auxiliary buffer that we will swap with primary on each iteration
        let mut aux_buffer = BytesMut::with_capacity((header_length + max_message_length) * 2);
        aux_buffer.resize(aux_buffer.capacity(), 0);

        loop {
            match stream.read(&mut buffer[buffer_contents_bytes..]).await {
                Ok(read_size) => {
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
                Err(error) => {
                    warn!("Failed to read bytes: {}", error);
                    break;
                }
            }
        }
    });

    messages_receiver
}

async fn exchange_peer_addr(own_addr: SocketAddr, stream: &mut TcpStream) -> Option<SocketAddr> {
    // TODO: Timeout for this function
    let own_addr_string = own_addr.to_string();
    if let Err(error) = stream
        .write(&[own_addr_string.as_bytes().len() as u8])
        .await
    {
        warn!("Failed to write node address length: {}", error);
        return None;
    }
    if let Err(error) = stream.write(own_addr_string.as_bytes()).await {
        warn!("Failed to write node address: {}", error);
        return None;
    }

    let mut peer_addr_len = [0];
    if let Err(error) = stream.read_exact(&mut peer_addr_len).await {
        warn!("Failed to read node address length: {}", error);
        return None;
    }
    let mut peer_addr_bytes = vec![0; peer_addr_len[0] as usize];
    if let Err(error) = stream.read_exact(&mut peer_addr_bytes).await {
        warn!("Failed to read node address: {}", error);
        return None;
    }

    let peer_addr_string = match String::from_utf8(peer_addr_bytes) {
        Ok(peer_addr_string) => peer_addr_string,
        Err(error) => {
            warn!("Failed to parse node address from bytes: {}", error);
            return None;
        }
    };

    match peer_addr_string.parse() {
        Ok(peer_addr) => Some(peer_addr),
        Err(error) => {
            warn!(
                "Failed to parse node address {}: {}",
                peer_addr_string, error
            );
            return None;
        }
    }
}

#[derive(Debug)]
pub enum ConnectionError {
    AlreadyConnected,
    FailedToExchangeAddress,
    IO { error: io::Error },
}

#[derive(Debug)]
pub(crate) enum RequestError {
    ConnectionClosed,
    // BadResponse,
    MessageTooLong,
    NoPeers,
    TimedOut,
}

struct RequestsContainer<T> {
    next_id: u32,
    handlers: HashMap<u32, async_oneshot::Sender<T>>,
}

impl<T> Default for RequestsContainer<T> {
    fn default() -> Self {
        Self {
            next_id: 0,
            handlers: HashMap::new(),
        }
    }
}

#[derive(Default)]
struct Handlers {
    gossip: AsyncMutex<Vec<Box<dyn Fn(&GossipMessage) + Send>>>,
}

#[derive(Clone)]
pub struct ConnectedPeer {
    addr: SocketAddr,
    sender: Sender<Bytes>,
}

#[derive(Default)]
struct Peers {
    /// Active established connections
    connections: HashMap<SocketAddr, ConnectedPeer>,
    /// All known peers
    peers: HashSet<SocketAddr>,
}

struct Inner {
    node_id: NodeID,
    peers_store: Arc<AsyncMutex<Peers>>,
    connections_handle: StdMutex<Option<JoinHandle<()>>>,
    handlers: Handlers,
    gossip_sender: async_channel::Sender<(SocketAddr, GossipMessage)>,
    gossip_receiver: StdMutex<Option<async_channel::Receiver<(SocketAddr, GossipMessage)>>>,
    request_sender: async_channel::Sender<(RequestMessage, async_oneshot::Sender<ResponseMessage>)>,
    request_receiver: StdMutex<
        Option<async_channel::Receiver<(RequestMessage, async_oneshot::Sender<ResponseMessage>)>>,
    >,
    requests_container: Arc<AsyncMutex<RequestsContainer<ResponseMessage>>>,
    internal_requests_container: Arc<AsyncMutex<RequestsContainer<InternalResponseMessage>>>,
    node_addr: SocketAddr,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Stop accepting new connections, this will also drop the listener and close the socket
        async_std::task::spawn(
            self.connections_handle
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .cancel(),
        );
    }
}

#[derive(Clone)]
pub struct Network {
    inner: Arc<Inner>,
}

impl Network {
    pub async fn new(node_id: NodeID, addr: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let (gossip_sender, gossip_receiver) =
            async_channel::bounded::<(SocketAddr, GossipMessage)>(32);
        let (request_sender, request_receiver) =
            async_channel::bounded::<(RequestMessage, async_oneshot::Sender<ResponseMessage>)>(32);
        let node_addr = listener.local_addr()?;

        let handlers = Handlers::default();
        let inner = Arc::new(Inner {
            node_id,
            peers_store: Arc::default(),
            connections_handle: StdMutex::default(),
            handlers,
            gossip_sender,
            gossip_receiver: StdMutex::new(Some(gossip_receiver)),
            request_sender,
            request_receiver: StdMutex::new(Some(request_receiver)),
            requests_container: Arc::default(),
            internal_requests_container: Arc::default(),
            node_addr,
        });

        let network = Self { inner };

        let connections_handle = {
            let network_weak = network.downgrade();

            async_std::task::spawn(async move {
                let mut connections = listener.incoming();

                info!("Listening on TCP socket for inbound connections");

                while let Some(stream) = connections.next().await {
                    info!("New inbound TCP connection initiated");

                    let mut stream = stream.unwrap();
                    if let Some(network) = network_weak.upgrade() {
                        async_std::task::spawn(async move {
                            if let Some(peer_addr) =
                                exchange_peer_addr(node_addr, &mut stream).await
                            {
                                drop(network.on_connected(peer_addr, stream).await);
                            };
                        });
                    } else {
                        break;
                    }
                }
            })
        };

        network
            .inner
            .connections_handle
            .lock()
            .unwrap()
            .replace(connections_handle);

        Ok(network)
    }

    pub fn address(&self) -> SocketAddr {
        self.inner.node_addr
    }

    /// Send a message to all peers
    pub(crate) async fn gossip(&self, message: GossipMessage) {
        for callback in self.inner.handlers.gossip.lock().await.iter() {
            callback(&message);
        }

        let message = Message::Gossip(message);
        let bytes = message.to_bytes();
        for connected_peer in self
            .inner
            .peers_store
            .lock()
            .await
            .connections
            .values()
            .cloned()
        {
            trace!("Sending a {} message to {}", message, connected_peer.addr);
            let bytes = bytes.clone();
            async_std::task::spawn(async move {
                connected_peer.sender.send(bytes).await;
            });
        }
    }

    /// Send a message to all but one peer (who sent you the message)
    pub(crate) async fn regossip(&self, sender: &SocketAddr, message: GossipMessage) {
        for callback in self.inner.handlers.gossip.lock().await.iter() {
            callback(&message);
        }

        let message = Message::Gossip(message);
        let bytes = message.to_bytes();
        for connected_peer in self
            .inner
            .peers_store
            .lock()
            .await
            .connections
            .values()
            .cloned()
        {
            if &connected_peer.addr != sender {
                trace!("Sending a {} message to {}", message, connected_peer.addr);
                let bytes = bytes.clone();
                async_std::task::spawn(async move {
                    connected_peer.sender.send(bytes).await;
                });
            }
        }
    }

    pub(crate) async fn request_blocks(
        &self,
        block_height: u64,
    ) -> Result<Vec<Block>, RequestError> {
        let response = self
            .request(RequestMessage::Blocks(BlocksRequest { block_height }))
            .await?;

        match response {
            ResponseMessage::Blocks(response) => Ok(response.blocks),
            // _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) fn get_gossip_receiver(
        &self,
    ) -> Option<async_channel::Receiver<(SocketAddr, GossipMessage)>> {
        self.inner.gossip_receiver.lock().unwrap().take()
    }

    pub(crate) fn get_requests_receiver(
        &self,
    ) -> Option<async_channel::Receiver<(RequestMessage, async_oneshot::Sender<ResponseMessage>)>>
    {
        self.inner.request_receiver.lock().unwrap().take()
    }

    pub(crate) async fn get_state(&self) -> console::AppState {
        let peers = self.inner.peers_store.lock().await;
        console::AppState {
            node_type: String::from(""),
            node_id: hex::encode(&self.inner.node_id[0..8]),
            node_addr: self.inner.node_addr.to_string(),
            connections: peers.connections.len().to_string(),
            peers: peers.peers.len().to_string(),
            pieces: String::from(""),
            blocks: String::from(""),
        }
    }

    pub(crate) async fn request_peers(
        &self,
        peer: ConnectedPeer,
    ) -> Result<Vec<SocketAddr>, RequestError> {
        let response = self
            .internal_request(peer, InternalRequestMessage::Peers)
            .await?;

        match response {
            InternalResponseMessage::Peers(peers) => Ok(peers),
            // _ => Err(RequestError::BadResponse),
        }
    }

    pub async fn on_gossip<F: Fn(&GossipMessage) + Send + 'static>(&self, callback: F) {
        self.inner
            .handlers
            .gossip
            .lock()
            .await
            .push(Box::new(callback));
    }

    fn downgrade(&self) -> NetworkWeak {
        let inner = Arc::downgrade(&self.inner);
        NetworkWeak { inner }
    }

    pub async fn connect_to(
        &self,
        peer_addr: SocketAddr,
    ) -> Result<ConnectedPeer, ConnectionError> {
        let mut stream = TcpStream::connect(peer_addr)
            .await
            .map_err(|error| ConnectionError::IO { error })?;
        let network = self.clone();

        match exchange_peer_addr(self.inner.node_addr, &mut stream).await {
            Some(peer_addr) => network.on_connected(peer_addr, stream).await,
            None => Err(ConnectionError::FailedToExchangeAddress),
        }
    }

    async fn on_connected(
        self,
        peer_addr: SocketAddr,
        mut stream: TcpStream,
    ) -> Result<ConnectedPeer, ConnectionError> {
        let (client_sender, mut client_receiver) = channel::<Bytes>(32);

        let connected_peer = {
            let mut peers_store = self.inner.peers_store.lock().await;

            if peers_store.connections.contains_key(&peer_addr) {
                return Err(ConnectionError::AlreadyConnected);
            }

            let connected_peer = ConnectedPeer {
                addr: peer_addr,
                sender: client_sender.clone(),
            };
            peers_store
                .connections
                .insert(peer_addr, connected_peer.clone());
            // if peers is low, add to peers
            // later explicitly ask to reduce churn
            if peers_store.peers.len() < MAX_PEERS {
                peers_store.peers.insert(peer_addr);
            }

            connected_peer
        };

        let mut messages_receiver = read_messages(stream.clone());

        // listen for new messages from the broker and send back to peer over stream
        async_std::task::spawn(async move {
            while let Some(bytes) = client_receiver.next().await {
                let length = bytes.len() as u16;
                let result: io::Result<()> = try {
                    stream.write_all(&length.to_le_bytes()).await?;
                    stream.write_all(&bytes).await?
                };
                if result.is_err() {
                    break;
                }
            }
        });

        async_std::task::spawn(async move {
            while let Some(message) = messages_receiver.next().await {
                if let Ok(message) = message {
                    match message {
                        Message::Gossip(message) => {
                            drop(self.inner.gossip_sender.send((peer_addr, message)).await);
                        }
                        Message::Request { id, message } => {
                            let (response_sender, response_receiver) = async_oneshot::oneshot();
                            drop(
                                self.inner
                                    .request_sender
                                    .send((message, response_sender))
                                    .await,
                            );
                            {
                                let client_sender = client_sender.clone();
                                async_std::task::spawn(async move {
                                    if let Ok(message) = response_receiver.await {
                                        drop(
                                            client_sender
                                                .send(Message::Response { id, message }.to_bytes())
                                                .await,
                                        );
                                    }
                                });
                            }
                        }
                        Message::Response { id, message } => {
                            if let Some(response_sender) = self
                                .inner
                                .requests_container
                                .lock()
                                .await
                                .handlers
                                .remove(&id)
                            {
                                drop(response_sender.send(message));
                            } else {
                                debug!("Received response for unknown request {}", id);
                            }
                        }
                        Message::InternalRequest { id, message } => {
                            let response = match message {
                                InternalRequestMessage::Peers => InternalResponseMessage::Peers(
                                    self.inner
                                        .peers_store
                                        .lock()
                                        .await
                                        .peers
                                        .iter()
                                        .filter(|&&address| {
                                            address != self.inner.node_addr && address != peer_addr
                                        })
                                        .copied()
                                        .collect(),
                                ),
                            };
                            drop(
                                client_sender
                                    .send(
                                        Message::InternalResponse {
                                            id,
                                            message: response,
                                        }
                                        .to_bytes(),
                                    )
                                    .await,
                            );
                        }
                        Message::InternalResponse { id, message } => {
                            if let Some(response_sender) = self
                                .inner
                                .internal_requests_container
                                .lock()
                                .await
                                .handlers
                                .remove(&id)
                            {
                                drop(response_sender.send(message));
                            } else {
                                debug!("Received response for unknown request {}", id);
                            }
                        }
                    }
                }
            }

            let mut peers_store = self.inner.peers_store.lock().await;

            peers_store.connections.remove(&peer_addr);
            peers_store.peers.remove(&peer_addr);
            info!("Broker has dropped a peer who disconnected");
        });

        Ok(connected_peer)
    }

    /// Non-generic method to avoid significant duplication in final binary
    async fn request(&self, message: RequestMessage) -> Result<ResponseMessage, RequestError> {
        let id;
        let (response_sender, response_receiver) = async_oneshot::oneshot();
        let requests_container = &self.inner.requests_container;

        {
            let mut requests_container = requests_container.lock().await;

            id = requests_container.next_id;

            requests_container.next_id = requests_container.next_id.wrapping_add(1);
            requests_container.handlers.insert(id, response_sender);
        }

        let message = Message::Request { id, message }.to_bytes();
        if message.len() > MAX_MESSAGE_CONTENTS_LENGTH {
            requests_container.lock().await.handlers.remove(&id);

            return Err(RequestError::MessageTooLong);
        }

        // TODO: Previous version of the code used peers instead of connections, was it correct?
        let connected_peer = self
            .inner
            .peers_store
            .lock()
            .await
            .connections
            .values()
            .choose(&mut rand::thread_rng())
            .cloned();
        if let Some(connected_peer) = connected_peer {
            async_std::task::spawn(async move {
                connected_peer.sender.send(message).await;
            });
        } else {
            return Err(RequestError::NoPeers);
        }

        future::or(
            async move {
                response_receiver
                    .await
                    .map_err(|_| RequestError::ConnectionClosed {})
            },
            async move {
                async_io::Timer::after(REQUEST_TIMEOUT).await;

                requests_container.lock().await.handlers.remove(&id);

                Err(RequestError::TimedOut)
            },
        )
        .await
    }

    /// Non-generic method to avoid significant duplication in final binary
    async fn internal_request(
        &self,
        peer: ConnectedPeer,
        message: InternalRequestMessage,
    ) -> Result<InternalResponseMessage, RequestError> {
        let id;
        let (response_sender, response_receiver) = async_oneshot::oneshot();
        let internal_requests_container = &self.inner.internal_requests_container;

        {
            let mut internal_requests_container = internal_requests_container.lock().await;

            id = internal_requests_container.next_id;

            internal_requests_container.next_id =
                internal_requests_container.next_id.wrapping_add(1);
            internal_requests_container
                .handlers
                .insert(id, response_sender);
        }

        let message = Message::InternalRequest { id, message }.to_bytes();
        if message.len() > MAX_MESSAGE_CONTENTS_LENGTH {
            internal_requests_container
                .lock()
                .await
                .handlers
                .remove(&id);

            return Err(RequestError::MessageTooLong);
        }

        async_std::task::spawn(async move {
            peer.sender.send(message).await;
        });

        future::or(
            async move {
                response_receiver
                    .await
                    .map_err(|_| RequestError::ConnectionClosed {})
            },
            async move {
                async_io::Timer::after(REQUEST_TIMEOUT).await;

                internal_requests_container
                    .lock()
                    .await
                    .handlers
                    .remove(&id);

                Err(RequestError::TimedOut)
            },
        )
        .await
    }

    pub(crate) async fn get_random_peer(&self) -> Option<ConnectedPeer> {
        self.inner
            .peers_store
            .lock()
            .await
            .connections
            .values()
            .choose(&mut rand::thread_rng())
            .cloned()
    }

    // /// retrieve the socket addr for each peer, except the one asking
    // fn _get_contacts(&self, exception: &SocketAddr) -> Vec<SocketAddr> {
    //     self.peers
    //         .iter()
    //         .filter(|&peer| !peer.eq(&exception))
    //         .copied()
    //         .collect()
    // }
}

#[derive(Clone)]
struct NetworkWeak {
    inner: Weak<Inner>,
}

impl NetworkWeak {
    fn upgrade(&self) -> Option<Network> {
        self.inner.upgrade().map(|inner| Network { inner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Block, Content, Proof};
    use crate::network::messages::BlocksResponse;
    use crate::transaction::{AccountAddress, CoinbaseTx};
    use crate::{ContentId, ProofId, Tag};
    use futures::executor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    fn fake_block() -> Block {
        Block {
            data: None,
            proof: Proof {
                randomness: ProofId::default(),
                epoch: 0,
                timeslot: 0,
                public_key: [0u8; 32],
                tag: Tag::default(),
                nonce: 0,
                piece_index: 0,
                solution_range: 0,
            },
            content: Content {
                proof_id: ProofId::default(),
                parent_id: ContentId::default(),
                uncle_ids: vec![],
                proof_signature: vec![],
                timestamp: 0,
                tx_ids: vec![],
                signature: vec![],
            },
            coinbase_tx: CoinbaseTx {
                reward: 0,
                to_address: AccountAddress::default(),
                proof_id: ProofId::default(),
            },
        }
    }

    #[test]
    fn test_create() {
        init();
        executor::block_on(async {
            Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");
        });
    }

    #[test]
    fn test_gossip_regossip_callback() {
        init();
        executor::block_on(async {
            let gateway_network = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");

            {
                let callback_called = Arc::new(AtomicUsize::new(0));

                {
                    let callback_called = Arc::clone(&callback_called);
                    gateway_network
                        .on_gossip(move |_message: &GossipMessage| {
                            callback_called.fetch_add(1, Ordering::SeqCst);
                        })
                        .await;
                }

                gateway_network
                    .gossip(GossipMessage::BlockProposal {
                        block: fake_block(),
                    })
                    .await;
                assert_eq!(
                    1,
                    callback_called.load(Ordering::SeqCst),
                    "Failed to fire gossip callback",
                );

                gateway_network
                    .regossip(
                        &"127.0.0.1:0".parse().unwrap(),
                        GossipMessage::BlockProposal {
                            block: fake_block(),
                        },
                    )
                    .await;
                assert_eq!(
                    2,
                    callback_called.load(Ordering::SeqCst),
                    "Failed to fire gossip callback",
                );
            }
        });
    }

    #[test]
    fn test_gossip_regossip() {
        init();
        executor::block_on(async {
            let gateway_network = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");
            let mut gateway_gossip = gateway_network.get_gossip_receiver().unwrap();

            let peer_network = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");

            peer_network
                .connect_to(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            {
                let callback_called = Arc::new(AtomicUsize::new(0));

                {
                    let callback_called = Arc::clone(&callback_called);
                    gateway_network
                        .on_gossip(move |_message: &GossipMessage| {
                            callback_called.fetch_add(1, Ordering::SeqCst);
                        })
                        .await;
                }

                {
                    let peer_network = peer_network.clone();
                    async_std::task::spawn(async move {
                        peer_network
                            .gossip(GossipMessage::BlockProposal {
                                block: fake_block(),
                            })
                            .await;
                    });
                }

                assert!(
                    matches!(
                        gateway_gossip.next().await,
                        Some((_, GossipMessage::BlockProposal { .. }))
                    ),
                    "Expected block proposal gossip massage",
                );

                {
                    let peer_network = peer_network.clone();
                    async_std::task::spawn(async move {
                        peer_network
                            .regossip(
                                &"127.0.0.1:0".parse().unwrap(),
                                GossipMessage::BlockProposal {
                                    block: fake_block(),
                                },
                            )
                            .await;
                    });
                }

                assert!(
                    matches!(
                        gateway_gossip.next().await,
                        Some((_, GossipMessage::BlockProposal { .. }))
                    ),
                    "Expected block proposal gossip massage",
                );
            }
        });
    }

    #[test]
    fn test_request_response() {
        init();
        executor::block_on(async {
            let gateway_network = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");
            let mut gateway_requests = gateway_network.get_requests_receiver().unwrap();

            let peer_network = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");

            peer_network
                .connect_to(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            {
                let (response_sender, response_receiver) = async_oneshot::oneshot::<Vec<Block>>();
                {
                    let peer_network = peer_network.clone();
                    async_std::task::spawn(async move {
                        let blocks = peer_network.request_blocks(0).await.unwrap();
                        response_sender.send(blocks).unwrap();
                    });
                }

                {
                    let (request, sender) = gateway_requests.next().await.unwrap();
                    assert!(
                        matches!(request, RequestMessage::Blocks(..)),
                        "Expected blocks request",
                    );

                    sender
                        .send(ResponseMessage::Blocks(BlocksResponse {
                            blocks: vec![fake_block()],
                        }))
                        .unwrap();
                }

                let blocks = response_receiver.await.unwrap();

                assert_eq!(vec![fake_block()], blocks, "Bad blocks response");
            }
        });
    }

    #[test]
    fn test_get_peers() {
        init();
        executor::block_on(async {
            let gateway_network = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");

            let peer_network_1 = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");

            peer_network_1
                .connect_to(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            let peer_network_2 = Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");

            peer_network_2
                .connect_to(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            let random_peer = peer_network_1
                .get_random_peer()
                .await
                .expect("Must be connected to gateway");
            let peers = peer_network_1
                .request_peers(random_peer)
                .await
                .expect("Should return peers");

            assert_eq!(vec![peer_network_2.address()], peers, "Bad list of peers");
        });
    }
}
