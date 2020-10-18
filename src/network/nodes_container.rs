use async_std::sync::Sender;
use bytes::Bytes;
use rand::seq::IteratorRandom;
use std::collections::HashMap;
use std::net::SocketAddr;

#[derive(Debug, Copy, Clone)]
pub(super) struct Contact {
    node_addr: SocketAddr,
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
    fn from(Contact { node_addr }: Contact) -> Self {
        PendingPeer { node_addr }
    }
}

impl From<Peer> for PendingPeer {
    fn from(Peer { node_addr, .. }: Peer) -> Self {
        PendingPeer { node_addr }
    }
}

impl PendingPeer {
    pub(super) fn address(&self) -> SocketAddr {
        self.node_addr
    }
}

#[derive(Debug, Clone)]
pub(super) struct Peer {
    node_addr: SocketAddr,
    bytes_sender: Sender<Bytes>,
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

impl Peer {
    pub(super) async fn send(&self, bytes: Bytes) {
        self.bytes_sender.send(bytes).await
    }
}

// TODO: Max values are ignored, but shouldn't be
#[derive(Clone)]
pub(super) struct NodesContainer {
    min_contacts: usize,
    max_contacts: usize,
    min_peers: usize,
    max_peers: usize,
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
    ) -> Self {
        Self {
            min_contacts,
            max_contacts,
            min_peers,
            max_peers,
            contacts: HashMap::new(),
            pending_peers: HashMap::new(),
            peers: HashMap::new(),
        }
    }

    /// Add contacts to the list (will skip contacts that are already connected or pending)
    pub(super) fn add_contacts(&mut self, contacts: &[SocketAddr]) -> ContactsLevel {
        let contacts_until_max = self.max_contacts - self.contacts.len();
        for node_addr in contacts.iter().take(contacts_until_max).copied() {
            if !(self.pending_peers.contains_key(&node_addr) || self.peers.contains_key(&node_addr))
            {
                self.contacts.insert(node_addr, Contact { node_addr });
            }
        }

        self.contacts_level()
    }

    /// State transition from Contact to PendingPeer for random known contact
    ///
    /// Returns None if contacts list is empty
    pub(super) fn connect_to_random_contact(&mut self) -> Option<(PendingPeer, ContactsLevel)> {
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

                let contacts_level = self.contacts_level();

                Some((pending_peer, contacts_level))
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
    ) -> Option<(PendingPeer, ContactsLevel)> {
        match self.contacts.remove(node_addr) {
            Some(contact) => {
                let pending_peer: PendingPeer = contact.into();
                self.pending_peers
                    .insert(pending_peer.node_addr, pending_peer);

                let contacts_level = self.contacts_level();

                Some((pending_peer, contacts_level))
            }
            None => None,
        }
    }

    /// State transition from Peer to PendingPeer in case of reconnection needed
    ///
    /// Returns None if such connected peer was not found
    pub(super) fn start_peer_reconnection(
        &mut self,
        peer: &Peer,
    ) -> Option<(PendingPeer, PeersLevel)> {
        match self.peers.remove(&peer.node_addr) {
            Some(peer) => {
                let pending_peer: PendingPeer = peer.into();
                self.pending_peers
                    .insert(pending_peer.node_addr, pending_peer);

                let peers_level = self.peers_level();

                Some((pending_peer, peers_level))
            }
            None => None,
        }
    }

    /// State transition from PendingPeer to Peer in case of successful connection attempt
    ///
    /// Returns None if such pending peer was not found
    pub(super) fn finish_successful_connection_attempt(
        &mut self,
        pending_peer: &PendingPeer,
        bytes_sender: Sender<Bytes>,
    ) -> Option<(Peer, PeersLevel)> {
        match self.pending_peers.remove(&pending_peer.node_addr) {
            Some(PendingPeer { node_addr }) => {
                let peer = Peer {
                    node_addr,
                    bytes_sender,
                };
                self.peers.insert(node_addr, peer.clone());

                let peers_level = self.peers_level();

                Some((peer, peers_level))
            }
            None => None,
        }
    }

    /// PendingPeer removal in case of failed connection attempt
    pub(super) fn finish_failed_connection_attempt(&mut self, pending_peer: &PendingPeer) {
        self.pending_peers.remove(&pending_peer.node_addr);
    }

    fn contacts_level(&self) -> ContactsLevel {
        ContactsLevel {
            min_contacts: self.contacts.len() >= self.min_contacts,
            max_contacts: self.contacts.len() >= self.max_contacts,
        }
    }

    fn peers_level(&self) -> PeersLevel {
        PeersLevel {
            min_peers: self.peers.len() >= self.min_peers,
            max_peers: self.peers.len() >= self.max_peers,
        }
    }
}
