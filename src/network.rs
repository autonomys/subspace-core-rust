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
mod nodes_container;

use crate::block::Block;
use crate::manager::GenesisConfig;
use crate::network::messages::{
    BlockRequestByContentId, BlockRequestByProofId, GenesisConfigRequest, InternalRequestMessage,
    InternalResponseMessage, PieceRequestById, PieceRequestByIndex, StateBlockRequestByHeight,
    StateBlockRequestById, TxRequestById,
};
use crate::network::nodes_container::{
    ContactsLevel, NodesContainer, Peer, PeersLevel, PendingPeer,
};
use crate::state::{
    BlockHeight, NetworkPieceBundleById, NetworkPieceBundleByIndex, StateBlock, StateBlockId,
};
use crate::transaction::{SimpleCreditTx, Transaction, TxId};
use crate::{console, ContentId, PieceIndex, ProofId};
use crate::{NodeID, PieceId};
use async_std::fs::{File, OpenOptions};
use async_std::net::{TcpListener, TcpStream};
use async_std::sync::{channel, Receiver};
use async_std::task::JoinHandle;
use async_trait::async_trait;
use backoff::backoff::Backoff;
use backoff::ExponentialBackoff;
use bytes::BytesMut;
use futures::io::SeekFrom;
use futures::lock::{Mutex as AsyncMutex, Mutex};
use futures::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, StreamExt};
use futures_lite::future;
use log::*;
use messages::{BlocksRequest, GossipMessage, Message, RequestMessage, ResponseMessage};
use rand::prelude::*;
use std::collections::HashMap;
use std::convert::TryInto;
use std::fmt::{Debug, Display};
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;
use std::{fmt, io, mem};

/* Todo
 *
 * Fix all unwrap calls
 * Ensure message size does not exceed 16k by the sender (already handled by receiver)
 * Handle empty block responses, currently that peer will randomly come again soon
 * Handle errors as results
 *

 RPC Methods

    Network
    1. method similar to reqeust_blocks (uncomment match error)
    2. Add request message
    3. Add a response message
    4. Uncomment RequestError (BadResponse)

    Manager
    1. Add RequestMessage and Response
    2. Call Request somewhere in code

*/

const MAX_MESSAGE_CONTENTS_LENGTH: usize = 2usize.pow(16) - 1;
// TODO: Consider adaptive request timeout for more efficient sync
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const INITIAL_BACKOFF_INTERVAL: Duration = Duration::from_secs(1);
const MAX_BACKOFF_INTERVAL: Duration = Duration::from_secs(60);
const BACKOFF_MULTIPLIER: f64 = 10_f64;

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

fn create_message_receiver(mut stream: TcpStream) -> Receiver<Message> {
    let (messages_sender, message_receiver) = channel(10);

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
                        if let Ok(message) = message {
                            messages_sender.send(message).await;
                        }
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

    message_receiver
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

fn handle_messages(
    network_weak: NetworkWeak,
    mut message_receiver: Receiver<Message>,
    peer_addr: SocketAddr,
    peer: Peer,
) {
    async_std::task::spawn(async move {
        while let Some(message) = message_receiver.next().await {
            // TODO: This is probably suboptimal, we can probably get rid of it if we have special
            //  method to disconnect from all peers
            let network = match network_weak.upgrade() {
                Some(network) => network,
                None => {
                    // Network instance was destroyed
                    return;
                }
            };
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
                        let peer = peer.clone();
                        async_std::task::spawn(async move {
                            if let Ok(message) = response_receiver.await {
                                peer.send(Message::Response { id, message }.to_bytes())
                                    .await;
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
                        InternalRequestMessage::Contacts => InternalResponseMessage::Contacts(
                            network
                                .inner
                                .nodes_container
                                .lock()
                                .await
                                .get_contacts()
                                .filter(|&address| {
                                    address != &network.inner.node_addr && address != &peer_addr
                                })
                                // TODO: Limit the number of nodes
                                .copied()
                                .collect(),
                        ),
                    };
                    peer.send(
                        Message::InternalResponse {
                            id,
                            message: response,
                        }
                        .to_bytes(),
                    )
                    .await;
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

        if let Some(network) = network_weak.upgrade() {
            async_std::task::spawn(async move {
                network.reconnect(peer_addr).await;
            });
        }
    });
}

#[derive(Debug)]
pub enum ConnectionError {
    AlreadyConnected,
    FailedToExchangeAddress,
    ContactsRequest,
    NoContact,
    NoPendingPeer,
    IO { error: io::Error },
}

#[derive(Debug)]
pub(crate) enum RequestError {
    ConnectionClosed,
    BadResponse,
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
    peer: AsyncMutex<Vec<Box<dyn Fn(&Peer) + Send>>>,
    gossip: AsyncMutex<Vec<Box<dyn Fn(&GossipMessage) + Send>>>,
}
struct Inner {
    node_id: NodeID,
    nodes_container: Arc<AsyncMutex<NodesContainer>>,
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
    create_backoff: Box<dyn (Fn() -> ExponentialBackoff) + Send + Sync + 'static>,
    maintain_peers_interval: Duration,
    contacts_file: Arc<AsyncMutex<File>>,
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

#[async_trait]
trait TransportCommon {
    fn nodes_container(&self) -> &AsyncMutex<NodesContainer>;

    fn node_addr(&self) -> SocketAddr;

    // TODO: It is ugly that we can get regular Network from StartupNetwork instance this way
    fn downgrade(&self) -> NetworkWeak;

    fn handlers(&self) -> &Handlers;

    fn internal_requests_container(
        &self,
    ) -> &AsyncMutex<RequestsContainer<InternalResponseMessage>>;

    // TODO: This function probably needs timeouts for various operations
    async fn connect_simple(&self, pending_peer: PendingPeer) -> Result<Peer, ConnectionError> {
        let mut stream = match TcpStream::connect(pending_peer.address()).await {
            Ok(stream) => stream,
            Err(error) => {
                self.on_connection_failure(&pending_peer).await;

                return Err(ConnectionError::IO { error });
            }
        };

        match exchange_peer_addr(self.node_addr(), &mut stream).await {
            Some(_peer_addr) => match self.on_connection_success(&pending_peer, stream).await {
                Some(peer) => Ok(peer),
                None => Err(ConnectionError::NoPendingPeer),
            },
            None => {
                self.on_connection_failure(&pending_peer).await;
                Err(ConnectionError::FailedToExchangeAddress)
            }
        }
    }

    async fn on_connection_success(
        &self,
        pending_peer: &PendingPeer,
        stream: TcpStream,
    ) -> Option<Peer> {
        let peer = self
            .nodes_container()
            .lock()
            .await
            .finish_successful_connection_attempt(pending_peer, stream.clone());

        if let Some(peer) = &peer {
            let message_receiver = create_message_receiver(stream);

            handle_messages(
                self.downgrade(),
                message_receiver,
                *peer.address(),
                peer.clone(),
            );

            for callback in self.handlers().peer.lock().await.iter() {
                callback(peer);
            }
        }

        peer
    }

    async fn on_connection_failure(&self, pending_peer: &PendingPeer) {
        self.nodes_container()
            .lock()
            .await
            .finish_failed_connection_attempt(pending_peer);
    }

    async fn sync_contacts(&self, peer: Peer) -> Result<(), RequestError> {
        if self
            .nodes_container()
            .lock()
            .await
            .contacts_level()
            .max_contacts()
        {
            // No need to request more contacts
            return Ok(());
        }
        let response = self
            .internal_request(peer, InternalRequestMessage::Contacts)
            .await?;

        match response {
            InternalResponseMessage::Contacts(contacts) => {
                for contact in contacts {
                    let network_weak = self.downgrade();
                    async_std::task::spawn(async move {
                        if let Ok(stream) = TcpStream::connect(contact).await {
                            drop(stream);
                            if let Some(network) = network_weak.upgrade() {
                                network
                                    .inner
                                    .nodes_container
                                    .lock()
                                    .await
                                    .add_contacts(&[contact]);
                            }
                        }
                    });
                }

                Ok(())
            } // _ => Err(RequestError::BadResponse),
        }
    }

    /// Non-generic method to avoid significant duplication in final binary
    async fn internal_request(
        &self,
        peer: Peer,
        message: InternalRequestMessage,
    ) -> Result<InternalResponseMessage, RequestError> {
        let id;
        let (response_sender, response_receiver) = async_oneshot::oneshot();
        let internal_requests_container = self.internal_requests_container();

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
            peer.send(message).await;
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

    async fn connect_to_random_contact(&self) -> Result<(), ConnectionError> {
        let pending_peer = match self
            .nodes_container()
            .lock()
            .await
            .connect_to_random_contact()
        {
            Some(pending_peer) => pending_peer,
            None => {
                return Err(ConnectionError::NoContact);
            }
        };

        match self.connect_simple(pending_peer).await {
            Ok(peer) => {
                drop(self.sync_contacts(peer).await);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

pub struct StartupNetwork {
    inner: Arc<Inner>,
}

impl TransportCommon for StartupNetwork {
    fn nodes_container(&self) -> &Mutex<NodesContainer> {
        &self.inner.nodes_container
    }

    fn node_addr(&self) -> SocketAddr {
        self.inner.node_addr
    }

    fn downgrade(&self) -> NetworkWeak {
        let inner = Arc::downgrade(&self.inner);
        NetworkWeak { inner }
    }

    fn handlers(&self) -> &Handlers {
        &self.inner.handlers
    }

    fn internal_requests_container(&self) -> &Mutex<RequestsContainer<InternalResponseMessage>> {
        &self.inner.internal_requests_container
    }
}

impl StartupNetwork {
    pub async fn new<CB>(
        node_id: NodeID,
        addr: SocketAddr,
        path: &PathBuf,
        min_peers: usize,
        max_peers: usize,
        min_contacts: usize,
        max_contacts: usize,
        block_list_size: usize,
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
        let contacts_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("contacts.json"))
            .await?;

        let inner = Arc::new(Inner {
            node_id,
            nodes_container: Arc::new(AsyncMutex::new(NodesContainer::new(
                min_contacts,
                max_contacts,
                min_peers,
                max_peers,
                block_list_size,
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
            create_backoff: Box::new(create_backoff),
            maintain_peers_interval,
            contacts_file: Arc::new(AsyncMutex::new(contacts_file)),
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
                            .nodes_container
                            .lock()
                            .await
                            .peers_level()
                            .max_peers()
                        {
                            // Ignore connection, we've reached a limit for connected peers
                            continue;
                        }
                        async_std::task::spawn(async move {
                            let peer_addr = match exchange_peer_addr(node_addr, &mut stream).await {
                                Some(peer_addr) => peer_addr,
                                None => {
                                    return;
                                }
                            };

                            let mut nodes_container = network.inner.nodes_container.lock().await;

                            if nodes_container.check_is_in_block_list(&peer_addr) {
                                return;
                            }
                            nodes_container.add_contacts(&[peer_addr]);

                            let pending_peer =
                                match nodes_container.connect_to_specific_contact(&peer_addr) {
                                    Some(pending_peer) => pending_peer,
                                    None => {
                                        return;
                                    }
                                };
                            drop(nodes_container);

                            if let Some(peer) =
                                network.on_connection_success(&pending_peer, stream).await
                            {
                                drop(network.sync_contacts(peer).await);
                            }
                        });
                    } else {
                        break;
                    }
                }
            })
        };

        {
            let mut background_tasks = network.inner.background_tasks.lock().unwrap();
            background_tasks.push(connections_handle);
        }

        Ok(network)
    }

    /// Connect during bootstrap process
    pub async fn connect(&self, peer_addr: SocketAddr) -> Result<ContactsLevel, ConnectionError> {
        // TODO: This function probably needs timeouts for various operations
        let mut nodes_container = self.inner.nodes_container.lock().await;

        nodes_container.add_contacts(&[peer_addr]);

        let pending_peer = match nodes_container.connect_to_specific_contact(&peer_addr) {
            Some(pending_peer) => pending_peer,
            None => {
                return Err(ConnectionError::NoContact);
            }
        };
        drop(nodes_container);

        match self.connect_simple(pending_peer).await {
            Ok(peer) => match self.sync_contacts(peer).await {
                Ok(_) => Ok(self.inner.nodes_container.lock().await.contacts_level()),
                Err(error) => {
                    debug!("Failed to request contacts from node: {:?}", error);
                    Err(ConnectionError::ContactsRequest)
                }
            },
            Err(error) => Err(error),
        }
    }

    pub async fn connect_to_random_contact(&self) -> Result<PeersLevel, ConnectionError> {
        match TransportCommon::connect_to_random_contact(self).await {
            Ok(_) => Ok(self.nodes_container().lock().await.peers_level()),
            Err(error) => Err(error),
        }
    }

    pub fn finish_startup(self) -> Network {
        Network::new(self.inner)
    }
}

#[derive(Clone)]
pub struct Network {
    inner: Arc<Inner>,
}

impl TransportCommon for Network {
    fn nodes_container(&self) -> &Mutex<NodesContainer> {
        &self.inner.nodes_container
    }

    fn node_addr(&self) -> SocketAddr {
        self.inner.node_addr
    }

    fn downgrade(&self) -> NetworkWeak {
        let inner = Arc::downgrade(&self.inner);
        NetworkWeak { inner }
    }

    fn handlers(&self) -> &Handlers {
        &self.inner.handlers
    }

    fn internal_requests_container(&self) -> &Mutex<RequestsContainer<InternalResponseMessage>> {
        &self.inner.internal_requests_container
    }
}

impl Network {
    fn new(inner: Arc<Inner>) -> Self {
        let maintain_peers_interval = inner.maintain_peers_interval;
        let network = Self { inner };

        let maintain_contacts_handle = async_std::task::spawn({
            let nodes_container = Arc::clone(&network.inner.nodes_container);
            let contacts_file = Arc::clone(&network.inner.contacts_file);
            let network_weak = network.downgrade();

            async move {
                let mut interval = async_std::stream::interval(maintain_peers_interval);
                while let Some(_) = interval.next().await {
                    let mut nodes_container_locked = nodes_container.lock().await;

                    let contacts = nodes_container_locked
                        .get_contacts()
                        .map(|addr| addr.to_string())
                        .collect::<Vec<_>>();

                    let result: io::Result<()> = try {
                        let mut contacts_file = contacts_file.lock().await;
                        let data = serde_json::to_vec(&contacts).unwrap();
                        contacts_file.seek(SeekFrom::Start(0)).await?;
                        contacts_file.write_all(&data).await?;
                        contacts_file.set_len(data.len() as u64).await?;
                        contacts_file.sync_all().await?;
                    };

                    if let Err(error) = result {
                        error!("Failed to persist contacts: {:?}", error);
                    }

                    for addr in nodes_container_locked.get_contacts_to_check() {
                        async_std::task::spawn({
                            let nodes_container = Arc::clone(&nodes_container);
                            async move {
                                match TcpStream::connect(addr).await {
                                    Ok(stream) => {
                                        drop(stream);
                                        nodes_container
                                            .lock()
                                            .await
                                            .finish_successful_contact_check(&addr);
                                    }
                                    Err(_error) => {
                                        nodes_container
                                            .lock()
                                            .await
                                            .finish_failed_contact_check(&addr);
                                    }
                                }
                            }
                        });
                    }

                    if !nodes_container_locked.contacts_level().min_contacts() {
                        let peer = (nodes_container_locked
                            .get_peers()
                            .choose(&mut rand::thread_rng())
                            as Option<&Peer>)
                            .cloned();
                        drop(nodes_container_locked);

                        if let Some(peer) = peer {
                            if let Some(network) = network_weak.upgrade() {
                                if let Err(error) = network.sync_contacts(peer).await {
                                    debug!("Failed to sync contacts on maintenance: {:?}", error);
                                }
                            }
                        } else {
                            warn!("Below min contacts and don't have any peers to request contacts from");
                        }
                    }
                }
            }
        });
        let maintain_peers_handle = async_std::task::spawn({
            let nodes_container = Arc::clone(&network.inner.nodes_container);
            let network_weak = network.downgrade();

            async move {
                let mut interval = async_std::stream::interval(maintain_peers_interval);
                while let Some(_) = interval.next().await {
                    if !nodes_container.lock().await.peers_level().min_peers() {
                        while let Some(network) = network_weak.upgrade() {
                            match network.connect_to_random_contact().await {
                                Ok(_) => {
                                    if nodes_container.lock().await.peers_level().min_peers() {
                                        // Got enough peers, good
                                        break;
                                    } else {
                                        continue;
                                    }
                                }
                                Err(error) => match error {
                                    ConnectionError::NoContact => {
                                        // No contacts left, give up
                                        warn!("No contacts left for peers maintenance below min peers");
                                        break;
                                    }
                                    _ => {
                                        // Try again with a different peer
                                        continue;
                                    }
                                },
                            }
                        }
                    }
                }
            }
        });
        {
            let mut background_tasks = network.inner.background_tasks.lock().unwrap();
            background_tasks.push(maintain_contacts_handle);
            background_tasks.push(maintain_peers_handle);
        }

        network
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
        for peer in self.inner.nodes_container.lock().await.get_peers().cloned() {
            // This line is just for IDE, otherwise it can't figure out the type
            let peer: Peer = peer;
            trace!("Sending a {} message to {}", message, peer.address());
            let bytes = bytes.clone();
            async_std::task::spawn(async move {
                peer.send(bytes).await;
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
        for peer in self
            .inner
            .nodes_container
            .lock()
            .await
            .get_peers()
            .filter(|peer| peer.address() != sender)
            .cloned()
        {
            // This line is just for IDE, otherwise it can't figure out the type
            let peer: Peer = peer;
            trace!("Sending a {} message to {}", message, peer.address());
            let bytes = bytes.clone();
            async_std::task::spawn(async move {
                peer.send(bytes).await;
            });
        }
    }

    pub(crate) async fn request_blocks(
        &self,
        timeslot: u64,
    ) -> Result<(Vec<Block>, Vec<SimpleCreditTx>), RequestError> {
        let response = self
            .request(RequestMessage::Blocks(BlocksRequest { timeslot }))
            .await?;

        match response {
            ResponseMessage::Blocks(response) => Ok((response.blocks, response.transactions)),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn _request_block_by_content_id(
        &self,
        id: ContentId,
    ) -> Result<Option<Block>, RequestError> {
        let response = self
            .request(RequestMessage::BlockByContentId(BlockRequestByContentId {
                id,
            }))
            .await?;

        match response {
            ResponseMessage::BlockByContentId(response) => Ok(response.block),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn _request_block_by_proof_id(
        &self,
        id: ProofId,
    ) -> Result<Option<Block>, RequestError> {
        let response = self
            .request(RequestMessage::BlockByProofId(BlockRequestByProofId { id }))
            .await?;

        match response {
            ResponseMessage::BlockByProofId(response) => Ok(response.block),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn _request_tx_by_id(
        &self,
        id: TxId,
    ) -> Result<Option<Transaction>, RequestError> {
        let response = self
            .request(RequestMessage::TransactionById(TxRequestById { id }))
            .await?;

        match response {
            ResponseMessage::TransactionById(response) => Ok(response.transaction),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn _request_piece_by_id(
        &self,
        id: PieceId,
    ) -> Result<Option<NetworkPieceBundleById>, RequestError> {
        let response = self
            .request(RequestMessage::PieceById(PieceRequestById { id }))
            .await?;

        match response {
            ResponseMessage::PieceById(response) => Ok(response.piece_bundle),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn request_piece_by_index(
        &self,
        index: PieceIndex,
    ) -> Result<Option<NetworkPieceBundleByIndex>, RequestError> {
        let response = self
            .request(RequestMessage::PieceByIndex(PieceRequestByIndex { index }))
            .await?;

        match response {
            ResponseMessage::PieceByIndex(response) => Ok(response.piece_bundle),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn _request_state_block_by_id(
        &self,
        id: StateBlockId,
    ) -> Result<Option<StateBlock>, RequestError> {
        let response = self
            .request(RequestMessage::StateById(StateBlockRequestById { id }))
            .await?;

        match response {
            ResponseMessage::StateById(response) => Ok(response.state_block),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn request_state_block_by_height(
        &self,
        height: BlockHeight,
    ) -> Result<Option<StateBlock>, RequestError> {
        let response = self
            .request(RequestMessage::StateByHeight(StateBlockRequestByHeight {
                height,
            }))
            .await?;

        match response {
            ResponseMessage::StateByHeight(response) => Ok(response.state_block),
            _ => Err(RequestError::BadResponse),
        }
    }

    pub(crate) async fn request_genesis_config(&self) -> Result<GenesisConfig, RequestError> {
        let response = self
            .request(RequestMessage::GenesisConfig(GenesisConfigRequest {}))
            .await?;

        match response {
            ResponseMessage::GenesisConfig(response) => Ok(response.genesis_config),
            _ => Err(RequestError::BadResponse),
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

    /// Add address to block list and drop active connection if there is any
    pub(super) async fn add_to_block_list(&self, node_addr: SocketAddr) {
        self.inner
            .nodes_container
            .lock()
            .await
            .add_to_block_list(node_addr);
    }

    pub(crate) async fn get_state(&self) -> console::AppState {
        let connections = self.inner.nodes_container.lock().await.get_peers().len();
        console::AppState {
            node_type: String::from(""),
            node_id: hex::encode(&self.inner.node_id[0..8]),
            node_addr: self.inner.node_addr.to_string(),
            connections: connections.to_string(),
            peers: "".to_string(),
            pieces: String::from(""),
            blocks: String::from(""),
        }
    }

    pub async fn on_peer<F: Fn(&Peer) + Send + 'static>(&self, callback: F) {
        self.inner
            .handlers
            .peer
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

        let peer = (self
            .inner
            .nodes_container
            .lock()
            .await
            .get_peers()
            // This is just for IDE that can't figure out type otherwise
            .choose(&mut rand::thread_rng()) as Option<&Peer>)
            .cloned();
        if let Some(peer) = peer {
            async_std::task::spawn(async move {
                peer.send(message).await;
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

    // TODO: This function probably needs timeouts for various operations
    async fn reconnect(&self, node_addr: SocketAddr) {
        let pending_peer = self
            .inner
            .nodes_container
            .lock()
            .await
            .start_peer_reconnection(&node_addr);
        if let Some(pending_peer) = pending_peer {
            let mut backoff = (self.inner.create_backoff)();
            while let Some(delay) = backoff.next_backoff() {
                debug!(
                    "Reconnecting to peer {:?} after {} seconds",
                    pending_peer,
                    delay.as_secs_f32(),
                );
                async_std::task::sleep(delay).await;

                let mut stream = match TcpStream::connect(pending_peer.address()).await {
                    Ok(stream) => stream,
                    Err(error) => {
                        debug!("Failed to reconnect to peer {:?}: {}", pending_peer, error);

                        continue;
                    }
                };

                match exchange_peer_addr(self.inner.node_addr, &mut stream).await {
                    Some(_peer_addr) => {
                        match self.on_connection_success(&pending_peer, stream).await {
                            Some(_peer) => {
                                debug!("Successfully reconnected to peer {:?}", pending_peer);
                                return;
                            }
                            None => {
                                debug!(
                                    "Failed to reconnect to peer {:?}: no pending peer",
                                    pending_peer,
                                );
                            }
                        }
                    }
                    None => {
                        debug!(
                            "Failed to reconnect to peer {:?}: failed to exchange address",
                            pending_peer,
                        );
                    }
                }
            }

            self.on_connection_failure(&pending_peer).await;

            if !self
                .inner
                .nodes_container
                .lock()
                .await
                .peers_level()
                .min_peers()
            {
                // Below min_peers, let's connect to someone
                loop {
                    // TODO: This can quickly exhaust contacts in case of temporary network
                    //  disruption, isn't this a problem?
                    // TODO: This will establish one connection, but it may also fail; there should
                    //  be a mechanism to establish connections when new contacts are requested and
                    //  we are below min peers
                    match self.connect_to_random_contact().await {
                        Ok(_) => {
                            // Connected, good, move on
                            break;
                        }
                        Err(error) => match error {
                            ConnectionError::NoContact => {
                                // No contacts left, give up
                                break;
                            }
                            _ => {
                                continue;
                            }
                        },
                    }
                }
            }
        }
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
    use crate::transaction::{AccountAddress, CoinbaseTx, SimpleCreditTx};
    use crate::{ContentId, ProofId, Tag};
    use futures::executor;
    use std::fs;
    use std::ops::Deref;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    struct TargetDirectory {
        path: PathBuf,
    }

    impl Drop for TargetDirectory {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.path));
        }
    }

    impl Deref for TargetDirectory {
        type Target = PathBuf;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl TargetDirectory {
        fn new(test_name: &str) -> Self {
            let path = PathBuf::from("target").join(test_name);

            fs::create_dir_all(&path).unwrap();

            Self { path }
        }
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
                parent_id: Some(ContentId::default()),
                proof_signature: vec![],
                timestamp: 0,
                refs: vec![],
                signature: vec![],
            },
            coinbase_tx: CoinbaseTx {
                reward: 0,
                to_address: AccountAddress::default(),
                proof_id: ProofId::default(),
            },
        }
    }

    fn fake_tx() -> SimpleCreditTx {
        SimpleCreditTx::new(0, [0u8; 32], 0, &crate::crypto::gen_keys_random())
    }

    #[test]
    fn test_create() {
        init();
        executor::block_on(async {
            let path = TargetDirectory::new("test_create");

            StartupNetwork::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                &path,
                1,
                2,
                5,
                10,
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
            let path = TargetDirectory::new("test_gossip_regossip_callback");

            let gateway_network = StartupNetwork::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                &path,
                1,
                2,
                5,
                10,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start")
            .finish_startup();

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
            let path_gateway = TargetDirectory::new("test_gossip_regossip_gateway");

            let gateway_network = StartupNetwork::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                &path_gateway,
                1,
                2,
                5,
                10,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start")
            .finish_startup();
            let mut gateway_gossip = gateway_network.get_gossip_receiver().unwrap();

            let path_peer = TargetDirectory::new("test_gossip_regossip_peer");

            let peer_startup_network = StartupNetwork::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                &path_peer,
                1,
                2,
                5,
                10,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            peer_startup_network
                .connect(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            let peer_network = peer_startup_network.finish_startup();

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
            let path_gateway = TargetDirectory::new("test_request_response_gateway");

            let gateway_network = StartupNetwork::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                &path_gateway,
                1,
                2,
                5,
                10,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start")
            .finish_startup();

            let mut gateway_requests = gateway_network.get_requests_receiver().unwrap();

            let path_peer = TargetDirectory::new("test_request_response_peer");

            let peer_startup_network = StartupNetwork::new(
                NodeID::default(),
                "127.0.0.1:0".parse().unwrap(),
                &path_peer,
                1,
                2,
                5,
                10,
                10,
                Duration::from_secs(60),
                create_backoff,
            )
            .await
            .expect("Network failed to start");

            peer_startup_network
                .connect(gateway_network.address())
                .await
                .expect("Failed to connect to gateway");

            let peer_network = peer_startup_network.finish_startup();

            {
                let fake_block = fake_block();
                let fake_tx = fake_tx();

                let (response_sender, response_receiver) =
                    async_oneshot::oneshot::<(Vec<Block>, Vec<SimpleCreditTx>)>();
                {
                    let peer_network = peer_network.clone();
                    async_std::task::spawn(async move {
                        let bundle = peer_network.request_blocks(0).await.unwrap();
                        response_sender.send(bundle).unwrap();
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
                            blocks: vec![fake_block.clone()],
                            transactions: vec![fake_tx.clone()],
                        }))
                        .unwrap();
                }

                let blocks = response_receiver.await.unwrap();

                assert_eq!(
                    (vec![fake_block], vec![fake_tx]),
                    blocks,
                    "Bad blocks response"
                );
            }
        });
    }

    //     #[test]
    //     fn test_get_peers() {
    //         init();
    //         executor::block_on(async {
    //             let gateway_network = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_secs(60),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             let peer_network_1 = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_secs(60),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             peer_network_1
    //                 .connect_to(gateway_network.address())
    //                 .await
    //                 .expect("Failed to connect to gateway");
    //
    //             let peer_network_2 = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_secs(60),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             peer_network_2
    //                 .connect_to(gateway_network.address())
    //                 .await
    //                 .expect("Failed to connect to gateway");
    //
    //             let random_peer = peer_network_1
    //                 .get_random_connected_peer()
    //                 .await
    //                 .expect("Must be connected to gateway");
    //             let peers = peer_network_1
    //                 .request_contacts(random_peer)
    //                 .await
    //                 .expect("Should return peers");
    //
    //             assert_eq!(vec![peer_network_2.address()], peers, "Bad list of peers");
    //         });
    //     }
    //
    //     #[test]
    //     fn test_peers_discovery() {
    //         init();
    //         executor::block_on(async {
    //             let gateway_network = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_secs(60),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             let gateway_addr = gateway_network.address();
    //
    //             let peer_network_1 = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_secs(60),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             peer_network_1
    //                 .connect_to(gateway_addr)
    //                 .await
    //                 .expect("Failed to connect to gateway");
    //
    //             let peer_network_2 = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 2,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_secs(60),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             let connected_peers = Arc::new(AtomicUsize::new(0));
    //             {
    //                 let connected_peers = Arc::clone(&connected_peers);
    //                 peer_network_2
    //                     .on_connected_peer({
    //                         move |_connected_peer| {
    //                             connected_peers.fetch_add(1, Ordering::SeqCst);
    //                         }
    //                     })
    //                     .await;
    //             }
    //
    //             let (second_peer_sender, second_peer_receiver) = async_oneshot::oneshot::<SocketAddr>();
    //             {
    //                 let second_peer_sender = StdMutex::new(Some(second_peer_sender));
    //                 peer_network_2
    //                     .on_peer({
    //                         move |peer| {
    //                             if peer != gateway_addr {
    //                                 if let Some(sender) = second_peer_sender.lock().unwrap().take() {
    //                                     drop(sender.send(peer));
    //                                 }
    //                             }
    //                         }
    //                     })
    //                     .await;
    //             }
    //
    //             peer_network_2
    //                 .connect_to(gateway_addr)
    //                 .await
    //                 .expect("Failed to connect to gateway");
    //
    //             peer_network_2
    //                 .connect_to(second_peer_receiver.await.unwrap())
    //                 .await
    //                 .expect("Failed to connect to the other peer");
    //
    //             assert_eq!(
    //                 2,
    //                 connected_peers.load(Ordering::SeqCst),
    //                 "Should have 2 peers connected",
    //             );
    //         });
    //     }
    //
    //     #[test]
    //     fn test_peers_maintenance() {
    //         init();
    //         executor::block_on(async {
    //             let gateway_network = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_millis(100),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             let gateway_addr = gateway_network.address();
    //
    //             let peer_network_1 = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_millis(100),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             peer_network_1
    //                 .connect_to(gateway_addr)
    //                 .await
    //                 .expect("Failed to connect to gateway");
    //
    //             let peer_network_1_address = peer_network_1.address();
    //
    //             let peer_network_2 = Network::new(
    //                 NodeID::default(),
    //                 "127.0.0.1:0".parse().unwrap(),
    //                 1,
    //                 2,
    //                 2,
    //                 10,
    //                 Duration::from_millis(100),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             let (second_peer_sender, second_peer_receiver) = async_oneshot::oneshot::<SocketAddr>();
    //             {
    //                 let second_peer_sender = StdMutex::new(Some(second_peer_sender));
    //                 peer_network_2
    //                     .on_peer({
    //                         move |peer| {
    //                             if peer != gateway_addr {
    //                                 if let Some(sender) = second_peer_sender.lock().unwrap().take() {
    //                                     drop(sender.send(peer));
    //                                 }
    //                             }
    //                         }
    //                     })
    //                     .await;
    //             }
    //
    //             peer_network_2
    //                 .connect_to(gateway_addr)
    //                 .await
    //                 .expect("Failed to connect to gateway");
    //
    //             drop(second_peer_receiver.await);
    //
    //             drop(peer_network_1);
    //
    //             async_std::task::sleep(Duration::from_millis(500)).await;
    //
    //             assert_eq!(
    //                 peer_network_2.pull_random_disconnected_node().await,
    //                 None,
    //                 "Must have no disconnected peers",
    //             );
    //
    //             let peer_network_1 = Network::new(
    //                 NodeID::default(),
    //                 peer_network_1_address,
    //                 1,
    //                 2,
    //                 5,
    //                 10,
    //                 Duration::from_millis(100),
    //                 create_backoff,
    //             )
    //             .await
    //             .expect("Network failed to start");
    //
    //             peer_network_1
    //                 .connect_to(gateway_addr)
    //                 .await
    //                 .expect("Failed to connect to gateway");
    //
    //             async_std::task::sleep(Duration::from_millis(500)).await;
    //
    //             assert!(
    //                 peer_network_2
    //                     .pull_random_disconnected_node()
    //                     .await
    //                     .is_some(),
    //                 "Must have disconnected peer received from gateway",
    //             );
    //         });
    //     }
    //
    //     // TODO: Unlock when reconnection is triggered
    //     // #[test]
    //     // fn test_reconnection() {
    //     //     init();
    //     //     executor::block_on(async {
    //     //         let gateway_network = Network::new(
    //     //             NodeID::default(),
    //     //             "127.0.0.1:0".parse().unwrap(),
    //     //             1,
    //     //             2,
    //     //             5,
    //     //             10,
    //     //             Duration::from_millis(100),
    //     //             || {
    //     //                 let mut backoff = ExponentialBackoff::default();
    //     //                 backoff.initial_interval = Duration::from_millis(100);
    //     //                 backoff.max_interval = Duration::from_secs(5);
    //     //                 backoff
    //     //             },
    //     //         )
    //     //         .await
    //     //         .expect("Network failed to start");
    //     //
    //     //         let gateway_addr = gateway_network.address();
    //     //
    //     //         let peer_network_1 = Network::new(
    //     //             NodeID::default(),
    //     //             "127.0.0.1:0".parse().unwrap(),
    //     //             1,
    //     //             2,
    //     //             5,
    //     //             10,
    //     //             Duration::from_millis(100),
    //     //             create_backoff,
    //     //         )
    //     //         .await
    //     //         .expect("Network failed to start");
    //     //
    //     //         peer_network_1
    //     //             .connect_to(gateway_addr)
    //     //             .await
    //     //             .expect("Failed to connect to gateway");
    //     //
    //     //         let peer_network_1_address = peer_network_1.address();
    //     //
    //     //         drop(peer_network_1);
    //     //
    //     //         async_std::task::sleep(Duration::from_millis(500)).await;
    //     //
    //     //         assert!(
    //     //             gateway_network.get_random_connected_peer().await.is_none(),
    //     //             "All peers must be disconnected",
    //     //         );
    //     //
    //     //         let peer_network_1 = Network::new(
    //     //             NodeID::default(),
    //     //             peer_network_1_address,
    //     //             1,
    //     //             2,
    //     //             5,
    //     //             10,
    //     //             Duration::from_millis(100),
    //     //             create_backoff,
    //     //         )
    //     //         .await
    //     //         .expect("Network failed to start");
    //     //
    //     //         async_std::task::sleep(Duration::from_millis(500)).await;
    //     //
    //     //         assert!(
    //     //             gateway_network.get_random_connected_peer().await.is_some(),
    //     //             "Must reconnect to peer that re-appeared on the network",
    //     //         );
    //     //     });
    //     // }
}
