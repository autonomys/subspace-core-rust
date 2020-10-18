use crate::NodeID;
use async_std::sync::{Mutex, Sender};
use bytes::Bytes;
use rand::seq::IteratorRandom;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug, Copy, Clone)]
pub(super) struct Contact {
    node_id: NodeID,
    node_addr: SocketAddr,
}

#[derive(Debug, Copy, Clone)]
pub(super) struct ContactsLevel {
    pub(super) min_contacts: bool,
    pub(super) max_contacts: bool,
}

#[derive(Debug, Copy, Clone)]
pub(super) struct PendingPeer {
    node_id: NodeID,
    node_addr: SocketAddr,
}

impl From<Contact> for PendingPeer {
    fn from(Contact { node_id, node_addr }: Contact) -> Self {
        PendingPeer { node_id, node_addr }
    }
}

impl From<Peer> for PendingPeer {
    fn from(
        Peer {
            node_id, node_addr, ..
        }: Peer,
    ) -> Self {
        PendingPeer { node_id, node_addr }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Peer {
    node_id: NodeID,
    node_addr: SocketAddr,
    sender: Sender<Bytes>,
}

#[derive(Debug, Copy, Clone)]
pub(super) struct PeersLevel {
    pub(super) min_peers: bool,
    pub(super) max_peers: bool,
}

impl Peer {
    pub(super) async fn send(&self, bytes: Bytes) {
        self.sender.send(bytes).await
    }
}

struct Inner {
    min_contacts: usize,
    max_contacts: usize,
    min_peers: usize,
    max_peers: usize,
    contacts: HashMap<NodeID, Contact>,
    pending_peers: HashMap<NodeID, PendingPeer>,
    peers: HashMap<NodeID, Peer>,
}

impl Inner {
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

#[derive(Clone)]
pub(super) struct NodesContainer {
    inner: Arc<Mutex<Inner>>,
}

impl NodesContainer {
    pub(super) fn new(
        min_contacts: usize,
        max_contacts: usize,
        min_peers: usize,
        max_peers: usize,
    ) -> Self {
        let inner = Inner {
            min_contacts,
            max_contacts,
            min_peers,
            max_peers,
            contacts: HashMap::new(),
            pending_peers: HashMap::new(),
            peers: HashMap::new(),
        };

        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    pub(super) async fn add_contacts(&self, contacts: &[(NodeID, SocketAddr)]) -> ContactsLevel {
        let mut inner = self.inner.lock().await;
        let contacts_until_max = inner.max_contacts - inner.contacts.len();
        for (node_id, node_addr) in contacts.iter().take(contacts_until_max).copied() {
            inner
                .contacts
                .insert(node_id, Contact { node_id, node_addr });
        }

        inner.contacts_level()
    }

    /// State transition from Contact to PendingPeer for random known contact
    ///
    /// Returns None if contacts list is empty
    pub(super) async fn connect_to_random_contact(&self) -> Option<(PendingPeer, ContactsLevel)> {
        let mut inner = self.inner.lock().await;
        let node_id = inner
            .contacts
            .keys()
            .choose(&mut rand::thread_rng())
            .copied();
        match node_id {
            Some(node_id) => {
                let contact = inner.contacts.remove(&node_id).unwrap();
                let pending_peer: PendingPeer = contact.into();
                inner
                    .pending_peers
                    .insert(pending_peer.node_id, pending_peer);

                let contacts_level = inner.contacts_level();

                Some((pending_peer, contacts_level))
            }
            None => None,
        }
    }

    /// State transition from Peer to PendingPeer in case of reconnection needed
    ///
    /// Returns None if such connected peer was not found
    pub(super) async fn start_peer_reconnection(
        &self,
        peer: &Peer,
    ) -> Option<(PendingPeer, PeersLevel)> {
        let mut inner = self.inner.lock().await;
        match inner.peers.remove(&peer.node_id) {
            Some(peer) => {
                let pending_peer: PendingPeer = peer.into();
                inner
                    .pending_peers
                    .insert(pending_peer.node_id, pending_peer);

                let peers_level = inner.peers_level();

                Some((pending_peer, peers_level))
            }
            None => None,
        }
    }

    /// State transition from PendingPeer to Peer in case of successful connection attempt
    ///
    /// Returns None if such pending peer was not found
    pub(super) async fn finish_successful_connection_attempt(
        &self,
        pending_peer: &PendingPeer,
        sender: Sender<Bytes>,
    ) -> Option<(Peer, PeersLevel)> {
        let mut inner = self.inner.lock().await;
        match inner.pending_peers.remove(&pending_peer.node_id) {
            Some(PendingPeer { node_id, node_addr }) => {
                let peer = Peer {
                    node_id,
                    node_addr,
                    sender,
                };
                inner.peers.insert(node_id, peer.clone());

                let peers_level = inner.peers_level();

                Some((peer, peers_level))
            }
            None => None,
        }
    }

    /// PendingPeer removal in case of failed connection attempt
    pub(super) async fn finish_failed_connection_attempt(&self, pending_peer: &PendingPeer) {
        self.inner
            .lock()
            .await
            .pending_peers
            .remove(&pending_peer.node_id);
    }
}
