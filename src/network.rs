pub(crate) mod messages;

use crate::block::Block;
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

struct Router {
    node_id: NodeID,
    node_addr: SocketAddr,
    connections: HashMap<SocketAddr, Sender<Bytes>>,
    peers: HashSet<SocketAddr>,
}

impl Router {
    /// create a new empty router
    fn new(node_id: NodeID, node_addr: SocketAddr) -> Router {
        Router {
            node_id,
            node_addr,
            connections: HashMap::new(),
            peers: HashSet::new(),
        }
    }

    /// add a new connection, possibly add a new peer
    fn add(&mut self, node_addr: SocketAddr, sender: Sender<Bytes>) {
        self.connections.insert(node_addr, sender);

        // if peers is low, add to peers
        // later explicitly ask to reduce churn
        if self.peers.len() < MAX_PEERS {
            self.peers.insert(node_addr);
        }
    }

    /// get a connection by node id
    fn get_connection(&self, node_addr: &SocketAddr) -> Option<&Sender<Bytes>> {
        self.connections.get(node_addr)
    }

    /// remove a connection and peer if connection is removed
    fn remove(&mut self, peer_addr: SocketAddr) {
        // ToDo: Add another peer to replace the removed one

        if self.connections.contains_key(&peer_addr) {
            self.connections.remove(&peer_addr);
            self.peers.remove(&peer_addr);
        }
    }

    /// Send a message to all peers
    fn gossip(&self, message: GossipMessage) {
        let message = Message::Gossip(message);
        let bytes = message.to_bytes();
        for node_addr in self.peers.iter() {
            trace!("Sending a {} message to {}", message, node_addr);
            self.maybe_send_bytes_to(node_addr, bytes.clone());
        }
    }

    /// Send a message to all but one peer (who sent you the message)
    fn regossip(&self, sender: &SocketAddr, message: GossipMessage) {
        let message = Message::Gossip(message);
        let bytes = message.to_bytes();
        for node_addr in self.peers.iter() {
            if node_addr != sender {
                trace!("Sending a {} message to {}", message, node_addr);
                self.maybe_send_bytes_to(node_addr, bytes.clone());
            }
        }
    }

    fn maybe_send_bytes_to(&self, addr: &SocketAddr, bytes: Bytes) {
        if let Some(client_sender) = self.get_connection(addr).cloned() {
            async_std::task::spawn(async move {
                client_sender.send(bytes).await;
            });
        }
    }

    /// get a peer at random
    fn get_random_peer(&self) -> Option<SocketAddr> {
        self.peers.iter().choose(&mut rand::thread_rng()).copied()
    }

    /// get a peer at random excluding a specific peer
    fn _get_random_peer_excluding(&self, node_addr: SocketAddr) -> Option<SocketAddr> {
        self.peers
            .iter()
            .filter(|&peer_addr| !peer_addr.eq(&node_addr))
            .choose(&mut rand::thread_rng())
            .copied()
    }

    /// retrieve the socket addr for each peer, except the one asking
    fn _get_contacts(&self, exception: &SocketAddr) -> Vec<SocketAddr> {
        self.peers
            .iter()
            .filter(|&peer| !peer.eq(&exception))
            .copied()
            .collect()
    }

    fn get_state(&self) -> console::AppState {
        console::AppState {
            node_type: String::from(""),
            node_id: hex::encode(&self.node_id[0..8]),
            node_addr: self.node_addr.to_string(),
            connections: self.connections.len().to_string(),
            peers: self.peers.len().to_string(),
            pieces: String::from(""),
            blocks: String::from(""),
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

#[derive(Debug)]
pub(crate) enum RequestError {
    ConnectionClosed,
    // BadResponse,
    MessageTooLong,
    NoPeers,
    TimedOut,
}

#[derive(Default)]
struct RequestsContainer {
    next_id: u32,
    handlers: HashMap<u32, async_oneshot::Sender<ResponseMessage>>,
}

#[derive(Default)]
struct Handlers {
    gossip: AsyncMutex<Vec<Box<dyn Fn(&GossipMessage) + Send>>>,
}

struct Inner {
    connections_handle: StdMutex<Option<JoinHandle<()>>>,
    handlers: Handlers,
    gossip_sender: async_channel::Sender<(SocketAddr, GossipMessage)>,
    gossip_receiver: StdMutex<Option<async_channel::Receiver<(SocketAddr, GossipMessage)>>>,
    request_sender: async_channel::Sender<(RequestMessage, async_oneshot::Sender<ResponseMessage>)>,
    request_receiver: StdMutex<
        Option<async_channel::Receiver<(RequestMessage, async_oneshot::Sender<ResponseMessage>)>>,
    >,
    requests_container: Arc<AsyncMutex<RequestsContainer>>,
    router: AsyncMutex<Router>,
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
        let requests_container = Arc::<AsyncMutex<RequestsContainer>>::default();
        let node_addr = listener.local_addr()?;
        let router = Router::new(node_id, node_addr);

        let handlers = Handlers::default();
        let inner = Arc::new(Inner {
            connections_handle: StdMutex::default(),
            handlers,
            gossip_sender,
            gossip_receiver: StdMutex::new(Some(gossip_receiver)),
            request_sender,
            request_receiver: StdMutex::new(Some(request_receiver)),
            requests_container,
            router: AsyncMutex::new(router),
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

                    let stream = stream.unwrap();
                    let peer_addr = stream.peer_addr().unwrap();
                    if let Some(network) = network_weak.upgrade() {
                        async_std::task::spawn(network.on_connected(peer_addr, stream));
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
        self.inner.router.lock().await.gossip(message);
    }

    /// Send a message to all but one peer (who sent you the message)
    pub(crate) async fn regossip(&self, sender: &SocketAddr, message: GossipMessage) {
        for callback in self.inner.handlers.gossip.lock().await.iter() {
            callback(&message);
        }
        self.inner.router.lock().await.regossip(sender, message);
    }

    pub(crate) async fn request_blocks(
        &self,
        block_height: u64,
    ) -> Result<Vec<Block>, RequestError> {
        let response = self
            .request_internal(RequestMessage::BlocksRequest(BlocksRequest {
                block_height,
            }))
            .await?;

        match response {
            ResponseMessage::BlocksResponse(response) => Ok(response.blocks),
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
        self.inner.router.lock().await.get_state()
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

    pub async fn connect_to(&self, peer_addr: SocketAddr) -> io::Result<()> {
        let stream = TcpStream::connect(peer_addr).await?;
        self.clone().on_connected(peer_addr, stream).await;

        Ok(())
    }

    async fn on_connected(self, peer_addr: SocketAddr, mut stream: TcpStream) {
        let (client_sender, mut client_receiver) = channel::<Bytes>(32);
        self.inner
            .router
            .lock()
            .await
            .add(peer_addr, client_sender.clone());

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
                    }
                }
            }

            self.inner.router.lock().await.remove(peer_addr);
            info!("Broker has dropped a peer who disconnected");
        });
    }

    /// Non-generic method to avoid significant duplication in final binary
    async fn request_internal(
        &self,
        message: RequestMessage,
    ) -> Result<ResponseMessage, RequestError> {
        let router = self.inner.router.lock().await;
        let peer = match router.get_random_peer() {
            Some(peer) => peer,
            None => {
                return Err(RequestError::NoPeers);
            }
        };

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

        // TODO: Should be a better method for this (maybe without router)
        router.maybe_send_bytes_to(&peer, message);
        drop(router);

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
        executor::block_on(async {
            Network::new(NodeID::default(), "127.0.0.1:0".parse().unwrap())
                .await
                .expect("Network failed to start");
        });
    }

    #[test]
    fn test_gossip_regossip_callback() {
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
                        matches!(request, RequestMessage::BlocksRequest(..)),
                        "Expected blocks request",
                    );

                    sender
                        .send(ResponseMessage::BlocksResponse(BlocksResponse {
                            blocks: vec![fake_block()],
                        }))
                        .unwrap();
                }

                let blocks = response_receiver.await.unwrap();

                assert_eq!(vec![fake_block()], blocks, "Bad blocks response");
            }
        });
    }
}
