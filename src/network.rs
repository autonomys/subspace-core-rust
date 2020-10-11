//! Network module
//!
//! Network module manages TCP connections to other nodes on Subspace network and is used to
//! exchange gossip as well as request/response messages.
//!
//! During the first startup network instance needs to connect to one or more gateway nodes on order
//! to discover more nodes on the network and establish more connections (for reliability,
//! performance and security purposes).
//!
//! Once connections to other nodes on the network are established, gateway nodes are no longer
//! required for operation (upon restart network will try to reconnect to previously known nodes;
//! TODO: not implemented at the moment), but may be used as a fallback if needed.
//!
//! Every connection starts with node address exchange (as remote address of incoming connection
//! will not match publicly reachable address), after which communication consists of binary
//! messages prepended by 2-byte little-endian message length header. Messages are Rust enums and
//! are encoded using [bincode](https://crates.io/crates/bincode) (TODO: will probably change in
//! future).
//!
//! There are 2 somewhat distinct kinds of messages:
//! 1) Gossip: broadcast messages about blocks and transactions that should be propagated across the
//!   network, received messages can be re-gossiped
//! 2) Request/response: sometimes node needs to request something from another node (a block for
//!   instance), in this case special request message is sent with an ID and matching response is
//!   expected back
//! 3) Internal request/response: some internal mechanisms of the network like maintaining peers
//!   require additional request/response messages that are not a part of the public API; they are
//!   processed completely internally, but otherwise are identical to public request/response
//!   messages
//!
//! Gossip messages
//! Gossip messages are sent using public API (specific for each message) of network instance and
//! behave as fire and forget. They are sent to all connected peers without any acknowledgement.
//! There is a channel exposed by network instance that allows reading received gossip messages for
//! further processing. Re-gossiping is decided externally to the network instance and can be
//! triggered the same way as regular gossip, but with original sender node excluded from the list
//! of connected peers that should receive gossip.
//!
//! Request/response
//! Request/response API on the network instance looks like a regular async function on one side and
//! a channel with incoming requests on the other side that produces pairs of request message and
//! one-shot channel through which response must be provided.
//!
//! In order to maintain connectivity with the rest of the network a background process is running
//! that periodically tries to establish a TCP connection with nodes it is aware of (but doesn't
//! have an active connection to) to make sure information is not stale.
//! Same process also checks if the network instance is below desired number of known nodes and
//! actively connected peers and will proactively try to request peers and establish necessary
//! connections.
//!
//! External RPC interface is not part of the network, but can be built using event handlers and
//! public methods provided.

pub(crate) mod messages;
mod peers_and_nodes;

use crate::block::Block;
use crate::console;
use crate::network::messages::{InternalRequestMessage, InternalResponseMessage};
use async_std::net::{TcpListener, TcpStream};
use async_std::stream;
use async_std::sync::{channel, Receiver, Sender};
use async_std::task::JoinHandle;
use backoff::ExponentialBackoff;
use bytes::{Bytes, BytesMut};
use futures::lock::Mutex as AsyncMutex;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use futures_lite::future;
use log::*;
use messages::{BlocksRequest, GossipMessage, Message, RequestMessage, ResponseMessage};
use peers_and_nodes::PeersAndNodes;
use rand::prelude::*;
use std::collections::HashMap;
use std::convert::TryInto;
use std::fmt::{Debug, Display};
use std::io::Write;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};
use std::{fmt, io, mem};

/* Todo
 *
 * Fix all unwrap calls
 * Ensure message size does not exceed 16k by the sender (already handled by receiver)
 * Handle empty block responses, currently that peer will randomly come again soon
 * Handle errors as results
 *

*/

const MAX_MESSAGE_CONTENTS_LENGTH: usize = 2usize.pow(16) - 1;
// TODO: Consider adaptive request timeout for more efficient sync
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const INITIAL_BACKOFF_INTERVAL: Duration = Duration::from_secs(1);
const MAX_BACKOFF_INTERVAL: Duration = Duration::from_secs(60);
const BACKOFF_MULTIPLIER: f64 = 10_f64;

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

pub fn create_backoff() -> ExponentialBackoff {
    let mut backoff = ExponentialBackoff::default();
    backoff.initial_interval = INITIAL_BACKOFF_INTERVAL;
    backoff.max_interval = MAX_BACKOFF_INTERVAL;
    backoff.multiplier = BACKOFF_MULTIPLIER;
    backoff
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
        trace!("Failed to write node address length: {}", error);
        return None;
    }
    if let Err(error) = stream.write(own_addr_string.as_bytes()).await {
        trace!("Failed to write node address: {}", error);
        return None;
    }

    let mut peer_addr_len = [0];
    if let Err(error) = stream.read_exact(&mut peer_addr_len).await {
        trace!("Failed to read node address length: {}", error);
        return None;
    }
    let mut peer_addr_bytes = vec![0; peer_addr_len[0] as usize];
    if let Err(error) = stream.read_exact(&mut peer_addr_bytes).await {
        trace!("Failed to read node address: {}", error);
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

fn request_more_peers(network_weak: NetworkWeak, connected_peer: ConnectedPeer) {
    async_std::task::spawn(async move {
        if let Some(network) = network_weak.upgrade() {
            match network.request_peers(connected_peer.clone()).await {
                Ok(peers) => {
                    for peer in peers
                        .into_iter()
                        .filter(|&peer| peer != network.inner.node_addr)
                    {
                        let mut peers_store = network.inner.peers_store.lock().await;
                        if peers_store.nodes.contains(&peer) {
                            continue;
                        }
                        peers_store.nodes.put(peer, None);
                        for callback in network.inner.handlers.peer.lock().await.iter() {
                            callback(peer);
                        }
                    }
                }
                Err(error) => {
                    debug!("Failed to get peers from new peer: {:?}", error);
                }
            }
        }
    });
}

async fn on_connected(
    network: Network,
    peer_addr: SocketAddr,
    mut stream: TcpStream,
) -> Result<ConnectedPeer, ConnectionError> {
    let (client_sender, mut client_receiver) = channel::<Bytes>(32);

    let connected_peer = {
        let mut peers_store = network.inner.peers_store.lock().await;

        let connected_peer = ConnectedPeer {
            addr: peer_addr,
            sender: client_sender.clone(),
        };

        if !peers_store.register_connected_peer(connected_peer.clone()) {
            return Err(ConnectionError::AlreadyConnected);
        }

        for callback in network.inner.handlers.peer.lock().await.iter() {
            callback(peer_addr);
        }

        // Request more peers if needed
        if peers_store.known_nodes() < network.inner.max_nodes {
            request_more_peers(network.downgrade(), connected_peer.clone());
        }

        connected_peer
    };

    for callback in network.inner.handlers.connected_peer.lock().await.iter() {
        callback(&connected_peer);
    }

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

    let network_weak = network.downgrade();
    async_std::task::spawn(async move {
        while let Some(message) = messages_receiver.next().await {
            // TODO: This is probably suboptimal, we can probably get rid of it if we have special
            //  method to disconnect from all peers
            let network = match network_weak.upgrade() {
                Some(network) => network,
                None => {
                    // Network instance was destroyed
                    return;
                }
            };
            if let Ok(message) = message {
                match message {
                    Message::Gossip(message) => {
                        drop(network.inner.gossip_sender.send((peer_addr, message)).await);
                    }
                    Message::Request { id, message } => {
                        let (response_sender, response_receiver) = async_oneshot::oneshot();
                        drop(
                            network
                                .inner
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
                        if let Some(response_sender) = network
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
                                network
                                    .inner
                                    .peers_store
                                    .lock()
                                    .await
                                    .nodes
                                    .iter()
                                    .filter(|(&address, instant)| {
                                        instant.is_some()
                                            && address != network.inner.node_addr
                                            && address != peer_addr
                                    })
                                    .map(|(&addr, _)| addr)
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
                        if let Some(response_sender) = network
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

        if let Some(network) = network_weak.upgrade() {
            let mut peers_store = network.inner.peers_store.lock().await;

            peers_store.mark_peer_as_dropped(network.clone(), peer_addr);

            // TODO: Fallback to bootstrap nodes in case we can't reconnect at all
        }
    });

    Ok(connected_peer)
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
    peer: AsyncMutex<Vec<Box<dyn Fn(SocketAddr) + Send>>>,
    connected_peer: AsyncMutex<Vec<Box<dyn Fn(&ConnectedPeer) + Send>>>,
    gossip: AsyncMutex<Vec<Box<dyn Fn(&GossipMessage) + Send>>>,
}

#[derive(Clone)]
pub struct ConnectedPeer {
    addr: SocketAddr,
    sender: Sender<Bytes>,
}

struct Inner {
    node_id: NodeID,
    peers_store: Arc<AsyncMutex<PeersAndNodes>>,
    background_tasks: StdMutex<Vec<JoinHandle<()>>>,
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
    min_connected_peers: usize,
    max_nodes: usize,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let background_tasks: Vec<JoinHandle<()>> =
            mem::take(self.background_tasks.lock().unwrap().as_mut());
        async_std::task::spawn(async move {
            // Stop all long-running background tasks
            for task in background_tasks {
                task.cancel().await;
            }
        });
    }
}

#[derive(Clone)]
pub struct Network {
    inner: Arc<Inner>,
}

impl Network {
    pub async fn new<CB>(
        node_id: NodeID,
        addr: SocketAddr,
        min_connected_peers: usize,
        max_connected_peers: usize,
        min_nodes: usize,
        max_nodes: usize,
        maintain_peers_interval: Duration,
        create_backoff: CB,
    ) -> io::Result<Self>
    where
        CB: (Fn() -> ExponentialBackoff) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(addr).await?;
        let (gossip_sender, gossip_receiver) =
            async_channel::bounded::<(SocketAddr, GossipMessage)>(32);
        let (request_sender, request_receiver) =
            async_channel::bounded::<(RequestMessage, async_oneshot::Sender<ResponseMessage>)>(32);
        let node_addr = listener.local_addr()?;

        let handlers = Handlers::default();
        let inner = Arc::new(Inner {
            node_id,
            peers_store: Arc::new(AsyncMutex::new(PeersAndNodes::new(
                min_connected_peers,
                max_connected_peers,
                min_nodes,
                max_nodes,
                create_backoff,
            ))),
            background_tasks: StdMutex::default(),
            handlers,
            gossip_sender,
            gossip_receiver: StdMutex::new(Some(gossip_receiver)),
            request_sender,
            request_receiver: StdMutex::new(Some(request_receiver)),
            requests_container: Arc::default(),
            internal_requests_container: Arc::default(),
            node_addr,
            min_connected_peers,
            max_nodes,
        });

        let network = Self { inner };

        let connections_handle = {
            let network_weak = network.downgrade();

            async_std::task::spawn(async move {
                let mut connections = listener.incoming();

                info!("Listening on TCP socket for inbound connections");

                while let Some(stream) = connections.next().await {
                    debug!("New inbound TCP connection initiated");

                    let mut stream = stream.unwrap();
                    if let Some(network) = network_weak.upgrade() {
                        if network
                            .inner
                            .peers_store
                            .lock()
                            .await
                            .has_max_connected_peers()
                        {
                            // Ignore connection, we've reached a limit for connected peers
                            continue;
                        }
                        async_std::task::spawn(async move {
                            if let Some(peer_addr) =
                                exchange_peer_addr(node_addr, &mut stream).await
                            {
                                drop(on_connected(network, peer_addr, stream).await);
                            }
                        });
                    } else {
                        break;
                    }
                }
            })
        };

        let maintain_peers_handle = {
            let network_weak = network.downgrade();

            async_std::task::spawn(async move {
                let mut interval = stream::interval(maintain_peers_interval);
                while let Some(_) = interval.next().await {
                    if let Some(network) = network_weak.upgrade() {
                        for node in network
                            .inner
                            .peers_store
                            .lock()
                            .await
                            .nodes
                            .iter()
                            .map(|(&addr, _)| addr)
                        {
                            // TODO: TCP connections will get stuck for a long time
                            // if peers_store.connected_or_dropped(&node) {
                            //     // Already connected to or dropped from, no need to check
                            //     continue;
                            // }

                            let peers_store = Arc::clone(&network.inner.peers_store);
                            async_std::task::spawn(async move {
                                // TODO: Timeout?
                                let result = TcpStream::connect(node).await.is_ok();

                                let mut peers_store = peers_store.lock().await;
                                if result {
                                    if let Some(instant) = peers_store.nodes.get_mut(&node) {
                                        if instant.is_none() {
                                            instant.replace(Instant::now());
                                        }
                                    }
                                } else {
                                    trace!("Dropping unreachable peer {}", node);
                                    // if peers_store.has_connected_peer(&node) {
                                    //     return;
                                    // }
                                    // TODO: Stop the connection (which may have stuck)
                                    peers_store.peers.remove(&node);
                                    if let Some(node) = peers_store.nodes.peek_mut(&node) {
                                        node.take();
                                    }
                                }
                            });
                        }

                        while !network
                            .inner
                            .peers_store
                            .lock()
                            .await
                            .has_enough_connected_peers()
                        {
                            trace!("Low on connections, trying to establish more");
                            if let Some(peer) = network.pull_random_disconnected_node().await {
                                // TODO: Probably count number of errors for peer and remove it if
                                //  fails all the time
                                drop(network.connect_to(peer).await);
                            } else {
                                break;
                            }
                        }

                        if !network
                            .inner
                            .peers_store
                            .lock()
                            .await
                            .has_enough_known_nodes()
                        {
                            trace!("Low on peers, trying to request more");
                            // TODO: We need to kill broken connection
                            if let Some(connected_peer) = network.get_random_connected_peer().await
                            {
                                request_more_peers(network_weak.clone(), connected_peer);
                            }
                        }
                    } else {
                        break;
                    }
                }
            })
        };

        {
            let mut background_tasks = network.inner.background_tasks.lock().unwrap();
            background_tasks.push(connections_handle);
            background_tasks.push(maintain_peers_handle);
        }

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
            .get_connected_peers()
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
            .get_connected_peers()
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

    pub(crate) async fn request_blocks(&self, timeslot: u64) -> Result<Vec<Block>, RequestError> {
        let response = self
            .request(RequestMessage::Blocks(BlocksRequest { timeslot }))
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
            connections: peers.get_connected_peers().len().to_string(),
            peers: peers.nodes.len().to_string(),
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

    pub async fn on_peer<F: Fn(SocketAddr) + Send + 'static>(&self, callback: F) {
        self.inner
            .handlers
            .peer
            .lock()
            .await
            .push(Box::new(callback));
    }

    pub async fn on_connected_peer<F: Fn(&ConnectedPeer) + Send + 'static>(&self, callback: F) {
        self.inner
            .handlers
            .connected_peer
            .lock()
            .await
            .push(Box::new(callback));
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

        match exchange_peer_addr(self.inner.node_addr, &mut stream).await {
            Some(peer_addr) => on_connected(self.clone(), peer_addr, stream).await,
            None => Err(ConnectionError::FailedToExchangeAddress),
        }
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
            .get_connected_peers()
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

    pub async fn get_random_connected_peer(&self) -> Option<ConnectedPeer> {
        self.inner
            .peers_store
            .lock()
            .await
            .get_connected_peers()
            .choose(&mut rand::thread_rng())
            .cloned()
    }

    pub async fn pull_random_disconnected_node(&self) -> Option<SocketAddr> {
        self.inner
            .peers_store
            .lock()
            .await
            .pull_random_disconnected_node()
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
            Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");
        });
    }

    #[test]
    fn test_gossip_regossip_callback() {
        init();
        executor::block_on(async {
            let gateway_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
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
            let gateway_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");
            let mut gateway_gossip = gateway_network.get_gossip_receiver().unwrap();

            let peer_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
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
            let gateway_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");
            let mut gateway_requests = gateway_network.get_requests_receiver().unwrap();

            let peer_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
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
            let gateway_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            let peer_network_1 = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            peer_network_1
                .connect_to(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            let peer_network_2 = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            peer_network_2
                .connect_to(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            let random_peer = peer_network_1
                .get_random_connected_peer()
                .await
                .expect("Must be connected to gateway");
            let peers = peer_network_1
                .request_peers(random_peer)
                .await
                .expect("Should return peers");

            assert_eq!(vec![peer_network_2.address()], peers, "Bad list of peers");
        });
    }

    #[test]
    fn test_peers_discovery() {
        init();
        executor::block_on(async {
            let gateway_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            let gateway_addr = gateway_network.address();

            let peer_network_1 = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            peer_network_1
                .connect_to(gateway_addr)
                .await
                .expect("Failed to connect to gateway");

            let peer_network_2 = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                2,
                2,
                5,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            let connected_peers = Arc::new(AtomicUsize::new(0));
            {
                let connected_peers = Arc::clone(&connected_peers);
                peer_network_2
                    .on_connected_peer({
                        move |_connected_peer| {
                            connected_peers.fetch_add(1, Ordering::SeqCst);
                        }
                    })
                    .await;
            }

            let (second_peer_sender, second_peer_receiver) = async_oneshot::oneshot::<SocketAddr>();
            {
                let second_peer_sender = StdMutex::new(Some(second_peer_sender));
                peer_network_2
                    .on_peer({
                        move |peer| {
                            if peer != gateway_addr {
                                if let Some(sender) = second_peer_sender.lock().unwrap().take() {
                                    drop(sender.send(peer));
                                }
                            }
                        }
                    })
                    .await;
            }

            peer_network_2
                .connect_to(gateway_addr)
                .await
                .expect("Failed to connect to gateway");

            peer_network_2
                .connect_to(second_peer_receiver.await.unwrap())
                .await
                .expect("Failed to connect to the other peer");

            assert_eq!(
                2,
                connected_peers.load(Ordering::SeqCst),
                "Should have 2 peers connected",
            );
        });
    }

    #[test]
    fn test_peers_maintenance() {
        init();
        executor::block_on(async {
            let gateway_network = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_millis(100),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            let gateway_addr = gateway_network.address();

            let peer_network_1 = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                5,
                10,
                Duration::from_millis(100),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            peer_network_1
                .connect_to(gateway_addr)
                .await
                .expect("Failed to connect to gateway");

            let peer_network_1_address = peer_network_1.address();

            let peer_network_2 = Network::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                1,
                2,
                2,
                10,
                Duration::from_millis(100),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            let (second_peer_sender, second_peer_receiver) = async_oneshot::oneshot::<SocketAddr>();
            {
                let second_peer_sender = StdMutex::new(Some(second_peer_sender));
                peer_network_2
                    .on_peer({
                        move |peer| {
                            if peer != gateway_addr {
                                if let Some(sender) = second_peer_sender.lock().unwrap().take() {
                                    drop(sender.send(peer));
                                }
                            }
                        }
                    })
                    .await;
            }

            peer_network_2
                .connect_to(gateway_addr)
                .await
                .expect("Failed to connect to gateway");

            drop(second_peer_receiver.await);

            drop(peer_network_1);

            async_std::task::sleep(Duration::from_millis(500)).await;

            assert_eq!(
                peer_network_2.pull_random_disconnected_node().await,
                None,
                "Must have no disconnected peers",
            );

            let peer_network_1 = Network::new(
                NodeID::default(),
                peer_network_1_address,
                1,
                2,
                5,
                10,
                Duration::from_millis(100),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            peer_network_1
                .connect_to(gateway_addr)
                .await
                .expect("Failed to connect to gateway");

            async_std::task::sleep(Duration::from_millis(500)).await;

            assert!(
                peer_network_2
                    .pull_random_disconnected_node()
                    .await
                    .is_some(),
                "Must have disconnected peer received from gateway",
            );
        });
    }

    // TODO: Unlock when reconnection is triggered
    // #[test]
    // fn test_reconnection() {
    //     init();
    //     executor::block_on(async {
    //         let gateway_network = Network::new(
    //             NodeID::default(),
    //             "127.0.0.1:0".parse().unwrap(),
    //             1,
    //             2,
    //             5,
    //             10,
    //             Duration::from_millis(100),
    //             || {
    //                 let mut backoff = ExponentialBackoff::default();
    //                 backoff.initial_interval = Duration::from_millis(100);
    //                 backoff.max_interval = Duration::from_secs(5);
    //                 backoff
    //             },
    //         )
    //         .await
    //         .expect("Network failed to start");
    //
    //         let gateway_addr = gateway_network.address();
    //
    //         let peer_network_1 = Network::new(
    //             NodeID::default(),
    //             "127.0.0.1:0".parse().unwrap(),
    //             1,
    //             2,
    //             5,
    //             10,
    //             Duration::from_millis(100),
    //             create_backoff,
    //         )
    //         .await
    //         .expect("Network failed to start");
    //
    //         peer_network_1
    //             .connect_to(gateway_addr)
    //             .await
    //             .expect("Failed to connect to gateway");
    //
    //         let peer_network_1_address = peer_network_1.address();
    //
    //         drop(peer_network_1);
    //
    //         async_std::task::sleep(Duration::from_millis(500)).await;
    //
    //         assert!(
    //             gateway_network.get_random_connected_peer().await.is_none(),
    //             "All peers must be disconnected",
    //         );
    //
    //         let peer_network_1 = Network::new(
    //             NodeID::default(),
    //             peer_network_1_address,
    //             1,
    //             2,
    //             5,
    //             10,
    //             Duration::from_millis(100),
    //             create_backoff,
    //         )
    //         .await
    //         .expect("Network failed to start");
    //
    //         async_std::task::sleep(Duration::from_millis(500)).await;
    //
    //         assert!(
    //             gateway_network.get_random_connected_peer().await.is_some(),
    //             "Must reconnect to peer that re-appeared on the network",
    //         );
    //     });
    // }
}
