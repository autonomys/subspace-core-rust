use crate::network::{ConnectedPeer, Network, NetworkWeak};
use async_std::net::SocketAddr;
use backoff::backoff::Backoff;
use backoff::ExponentialBackoff;
use log::*;
use lru::LruCache;
use rand::seq::IteratorRandom;
use std::collections::hash_map::Values;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const INITIAL_BACKOFF_INTERVAL: Duration = Duration::from_secs(1);
const MAX_BACKOFF_INTERVAL: Duration = Duration::from_secs(60);
const BACKOFF_MULTIPLIER: f64 = 10_f64;

// TODO: Instead of hardcoded, this should be customizable
fn create_backoff() -> ExponentialBackoff {
    let mut backoff = ExponentialBackoff::default();
    backoff.initial_interval = INITIAL_BACKOFF_INTERVAL;
    backoff.max_interval = MAX_BACKOFF_INTERVAL;
    backoff.multiplier = BACKOFF_MULTIPLIER;
    backoff
}

fn try_to_reconnect(network_weak: NetworkWeak, peer_addr: SocketAddr) {
    // TODO: Should be possible to cancel this background task early
    async_std::task::spawn(async move {
        loop {
            let next_backoff = {
                let network = match network_weak.upgrade() {
                    Some(network) => network,
                    None => {
                        break;
                    }
                };
                let next_backoff = network
                    .inner
                    .peers_store
                    .lock()
                    .await
                    .dropped_peers
                    .get_mut(&peer_addr)
                    .and_then(|backoff| backoff.next_backoff());

                match next_backoff {
                    Some(next_backoff) => next_backoff,
                    None => {
                        break;
                    }
                }
            };

            async_std::task::sleep(next_backoff).await;

            debug!("Trying to reconnect to peer {}", peer_addr);

            if let Some(network) = network_weak.upgrade() {
                match network.connect_to(peer_addr).await {
                    Ok(_) => {
                        break;
                    }
                    Err(error) => {
                        debug!("Failed to reconnect to peer {}: {:?}", peer_addr, error);
                    }
                }
            } else {
                break;
            }
        }
    });
}

pub(super) struct PeersAndNodes {
    min_connected_peers: usize,
    max_connected_peers: usize,
    min_nodes: usize,
    /// Actively connected peers
    pub(super) peers: HashMap<SocketAddr, ConnectedPeer>,
    dropped_peers: HashMap<SocketAddr, ExponentialBackoff>,
    /// All known nodes on the network
    pub(super) nodes: LruCache<SocketAddr, Option<Instant>>,
}

impl PeersAndNodes {
    pub(super) fn new(
        min_connected_peers: usize,
        max_connected_peers: usize,
        min_nodes: usize,
        max_nodes: usize,
    ) -> Self {
        Self {
            min_connected_peers,
            max_connected_peers,
            min_nodes,
            peers: HashMap::new(),
            dropped_peers: HashMap::new(),
            nodes: LruCache::new(max_nodes),
        }
    }

    pub(super) fn has_connected_peer(&self, node: &SocketAddr) -> bool {
        self.peers.contains_key(node)
    }

    pub(super) fn get_connected_peers(&self) -> Values<SocketAddr, ConnectedPeer> {
        self.peers.values()
    }

    pub(super) fn connected_or_dropped(&self, node: &SocketAddr) -> bool {
        self.peers.contains_key(node) || self.dropped_peers.contains_key(node)
    }

    pub(super) fn has_enough_connected_peers(&self) -> bool {
        self.peers.len() >= self.min_connected_peers
    }

    pub(super) fn has_max_connected_peers(&self) -> bool {
        self.peers.len() >= self.max_connected_peers
    }

    pub(super) fn has_enough_known_nodes(&self) -> bool {
        self.nodes.len() >= self.min_nodes
    }

    /// Removes and returns random disconnected node
    pub(super) fn get_number_of_disconnected_nodes(&mut self) -> usize {
        self.nodes
            .iter()
            .map(|(addr, _)| addr)
            .filter(|addr| !self.connected_or_dropped(addr))
            .count()
    }

    /// Removes and returns random disconnected node
    pub(super) fn pull_random_disconnected_node(&mut self) -> Option<SocketAddr> {
        let node = self
            .nodes
            .iter()
            .map(|(addr, _)| addr)
            .filter(|addr| !self.connected_or_dropped(addr))
            .choose(&mut rand::thread_rng())
            .cloned();

        if let Some(node) = &node {
            self.nodes.pop(node);
        }

        node
    }

    /// Returns `false` if peer already exists and was not registered
    pub(super) fn register_connected_peer(&mut self, connected_peer: ConnectedPeer) -> bool {
        let peer_addr = connected_peer.addr;

        if self.peers.contains_key(&peer_addr) {
            return false;
        }

        self.peers.insert(peer_addr, connected_peer);
        self.dropped_peers.remove(&peer_addr);

        self.nodes.put(peer_addr, Some(Instant::now()));

        true
    }

    pub(super) fn mark_peer_as_dropped(&mut self, network: Network, peer_addr: SocketAddr) {
        self.peers.remove(&peer_addr);
        self.nodes.pop(&peer_addr);
        info!("Broker has dropped a peer who disconnected");

        // TODO: avoid inspecting inner
        if self.peers.len() < network.inner.min_connected_peers {
            self.dropped_peers.insert(peer_addr, create_backoff());
            try_to_reconnect(network.downgrade(), peer_addr);
        }
    }

    pub(super) fn known_nodes(&self) -> usize {
        self.nodes.len()
    }
}
