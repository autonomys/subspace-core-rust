use crate::network::PersistedNodes;
use async_std::net::{Shutdown, TcpStream};
use async_std::sync::{channel, Sender};
use bytes::Bytes;
use futures::{AsyncWriteExt, StreamExt};
use lru::LruCache;
use rand::seq::IteratorRandom;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Weak};

fn create_bytes_sender(mut stream: TcpStream) -> Sender<Bytes> {
    let (bytes_sender, mut bytes_receiver) = channel::<Bytes>(32);

    async_std::task::spawn(async move {
        while let Some(bytes) = bytes_receiver.next().await {
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

    bytes_sender
}

#[derive(Debug, Copy, Clone)]
pub(super) struct Contact {
    node_addr: SocketAddr,
    currently_checking: bool,
}

#[derive(Debug, Copy, Clone)]
pub struct ContactsLevel {
    min_contacts: bool,
    max_contacts: bool,
}

impl ContactsLevel {
    pub fn min_contacts(&self) -> bool {
        self.min_contacts
    }
    pub fn max_contacts(&self) -> bool {
        self.max_contacts
    }
}

#[derive(Debug, Copy, Clone)]
pub(super) struct PendingPeer {
    node_addr: SocketAddr,
}

impl From<Contact> for PendingPeer {
    fn from(Contact { node_addr, .. }: Contact) -> Self {
        PendingPeer { node_addr }
    }
}

impl From<Peer> for PendingPeer {
    fn from(peer: Peer) -> Self {
        PendingPeer {
            node_addr: peer.inner.node_addr,
        }
    }
}

impl PendingPeer {
    pub(super) fn address(&self) -> SocketAddr {
        self.node_addr
    }
}

#[derive(Debug, Clone)]
struct PeerInner {
    node_addr: SocketAddr,
    bytes_sender: Sender<Bytes>,
    stream: TcpStream,
}

impl Drop for PeerInner {
    fn drop(&mut self) {
        drop(self.stream.shutdown(Shutdown::Both));
    }
}

#[derive(Debug, Clone)]
pub struct Peer {
    inner: Arc<PeerInner>,
}

impl Peer {
    pub fn address(&self) -> &SocketAddr {
        &self.inner.node_addr
    }

    pub(super) async fn send(&self, bytes: Bytes) {
        self.inner.bytes_sender.send(bytes).await
    }

    pub fn downgrade(&self) -> PeerWeak {
        PeerWeak {
            inner: Arc::downgrade(&self.inner),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeerWeak {
    inner: Weak<PeerInner>,
}

impl PeerWeak {
    pub fn upgrade(&self) -> Option<Peer> {
        self.inner.upgrade().map(|inner| Peer { inner })
    }
}

#[derive(Debug, Copy, Clone)]
pub struct PeersLevel {
    min_peers: bool,
    max_peers: bool,
}

impl PeersLevel {
    pub fn min_peers(&self) -> bool {
        self.min_peers
    }
    pub fn max_peers(&self) -> bool {
        self.max_peers
    }
}

// TODO: Max values are ignored, but shouldn't be
pub(super) struct NodesContainer {
    min_contacts: usize,
    max_contacts: usize,
    min_peers: usize,
    max_peers: usize,
    block_list: LruCache<SocketAddr, ()>,
    contacts: HashMap<SocketAddr, Contact>,
    pending_peers: HashMap<SocketAddr, PendingPeer>,
    peers: HashMap<SocketAddr, Peer>,
}

impl NodesContainer {
    pub(super) fn new(
        min_contacts: usize,
        max_contacts: usize,
        min_peers: usize,
        max_peers: usize,
        block_list_size: usize,
    ) -> Self {
        Self {
            min_contacts,
            max_contacts,
            min_peers,
            max_peers,
            block_list: LruCache::new(block_list_size),
            contacts: HashMap::new(),
            pending_peers: HashMap::new(),
            peers: HashMap::new(),
        }
    }

    /// Add address to block list and drop active connection if there is any
    pub(super) fn add_to_block_list(&mut self, node_addr: SocketAddr) {
        self.block_list.put(node_addr, ());
        self.contacts.remove(&node_addr);
        self.pending_peers.remove(&node_addr);
        self.peers.remove(&node_addr);
    }

    pub(super) fn check_is_in_block_list(&mut self, node_addr: &SocketAddr) -> bool {
        self.block_list.get(node_addr).is_some()
    }

    /// Returns all known contacts, including those that are already connected or pending
    pub(super) fn get_contacts(&self) -> impl Iterator<Item = &SocketAddr> {
        // TODO: Should we prefer peers here (load balancing)
        self.peers
            .keys()
            .chain(self.pending_peers.keys())
            .chain(self.contacts.keys())
    }

    /// Returns all known contacts, actively connected peers and blocklist
    pub(super) fn get_persisted_nodes(&self) -> PersistedNodes {
        PersistedNodes {
            contacts: self
                .contacts
                .keys()
                .chain(self.pending_peers.keys())
                .copied()
                .collect(),
            peers: self.peers.keys().copied().collect(),
            blocklist: self.block_list.iter().map(|(&addr, _)| addr).collect(),
        }
    }

    /// Returns all known contacts, including those that are already connected or pending
    pub(super) fn get_contacts_to_check(&mut self) -> impl Iterator<Item = SocketAddr> + '_ {
        // TODO: Should we prefer peers here (load balancing)
        self.contacts.iter_mut().filter_map(|(addr, contact)| {
            if contact.currently_checking {
                None
            } else {
                contact.currently_checking = true;
                Some(*addr)
            }
        })
    }

    /// Add contacts to the list (will skip contacts that are already connected or pending)
    pub(super) fn add_contacts(&mut self, contacts: &[SocketAddr]) {
        let contacts_until_max = self.max_contacts - self.contacts.len();
        for node_addr in contacts.iter().take(contacts_until_max).copied() {
            if !(self.pending_peers.contains_key(&node_addr)
                || self.peers.contains_key(&node_addr)
                || self.check_is_in_block_list(&node_addr))
            {
                self.contacts.insert(
                    node_addr,
                    Contact {
                        node_addr,
                        currently_checking: false,
                    },
                );
            }
        }
    }

    pub(super) fn finish_successful_contact_check(&mut self, addr: &SocketAddr) {
        if let Some(contact) = self.contacts.get_mut(addr) {
            contact.currently_checking = false;
        }
    }

    pub(super) fn finish_failed_contact_check(&mut self, addr: &SocketAddr) {
        self.contacts.remove(addr);
    }

    /// State transition from Contact to PendingPeer for random known contact
    ///
    /// Returns None if contacts list is empty
    pub(super) fn connect_to_random_contact(&mut self) -> Option<PendingPeer> {
        let node_addr = self
            .contacts
            .keys()
            .choose(&mut rand::thread_rng())
            .copied();
        match node_addr {
            Some(node_addr) => {
                let contact = self.contacts.remove(&node_addr).unwrap();
                let pending_peer: PendingPeer = contact.into();
                self.pending_peers
                    .insert(pending_peer.node_addr, pending_peer);

                Some(pending_peer)
            }
            None => None,
        }
    }

    /// State transition from Contact to PendingPeer for a specific contact
    ///
    /// Returns None if contact in not present in the list
    pub(super) fn connect_to_specific_contact(
        &mut self,
        node_addr: &SocketAddr,
    ) -> Option<PendingPeer> {
        match self.contacts.remove(node_addr) {
            Some(contact) => {
                let pending_peer: PendingPeer = contact.into();
                self.pending_peers
                    .insert(pending_peer.node_addr, pending_peer);

                Some(pending_peer)
            }
            None => None,
        }
    }

    /// State transition from Peer to PendingPeer in case of reconnection needed
    ///
    /// Returns None if such connected peer was not found
    pub(super) fn start_peer_reconnection(
        &mut self,
        node_addr: &SocketAddr,
    ) -> Option<PendingPeer> {
        match self.peers.remove(&node_addr) {
            Some(peer) => {
                let pending_peer: PendingPeer = peer.into();
                self.pending_peers
                    .insert(pending_peer.node_addr, pending_peer);

                Some(pending_peer)
            }
            None => None,
        }
    }

    /// Remove peer and do nothing else
    pub(super) fn remove_peer(&mut self, node_addr: &SocketAddr) {
        self.peers.remove(&node_addr);
    }

    /// State transition from PendingPeer to Peer in case of successful connection attempt
    ///
    /// Returns None if such pending peer was not found
    pub(super) fn finish_successful_connection_attempt(
        &mut self,
        pending_peer: &PendingPeer,
        stream: TcpStream,
    ) -> Option<Peer> {
        let bytes_sender = create_bytes_sender(stream.clone());
        match self.pending_peers.remove(&pending_peer.node_addr) {
            Some(PendingPeer { node_addr }) => {
                let inner = PeerInner {
                    node_addr,
                    bytes_sender,
                    stream,
                };
                let peer = Peer {
                    inner: Arc::new(inner),
                };

                self.peers.insert(node_addr, peer.clone());

                Some(peer)
            }
            None => None,
        }
    }

    pub(super) fn is_peer_connected(&self, addr: &SocketAddr) -> bool {
        self.peers.contains_key(addr)
    }

    /// PendingPeer removal in case of failed connection attempt
    pub(super) fn finish_failed_connection_attempt(&mut self, pending_peer: &PendingPeer) {
        self.pending_peers.remove(&pending_peer.node_addr);
    }

    pub(super) fn get_peers(&self) -> impl ExactSizeIterator<Item = &Peer> {
        self.peers.values()
    }

    pub(super) fn contacts_level(&self) -> ContactsLevel {
        // TODO: Should this include connected and pending peers?
        ContactsLevel {
            min_contacts: self.contacts.len() >= self.min_contacts,
            max_contacts: self.contacts.len() >= self.max_contacts,
        }
    }

    pub(super) fn peers_level(&self) -> PeersLevel {
        PeersLevel {
            min_peers: self.peers.len() >= self.min_peers,
            max_peers: self.peers.len() >= self.max_peers,
        }
    }
}
