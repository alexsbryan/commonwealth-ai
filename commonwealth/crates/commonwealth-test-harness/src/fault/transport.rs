// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FaultTransport`] — a [`PeerTransport`] that resolves peer endpoints from
//! an explicit routing table and applies the shared [`FaultPolicy`].
//!
//! Installed per node via `AppState::install_peer_transport`, it is the single
//! choke point for *all* peer dialing (gossip + knowledge fan-out both resolve
//! through `AppState::peer_transport()`). It owns its own `node_id -> addr`
//! table, so simulated member records need no real addresses.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use commonwealth_core::ids::NodeId;
use commonwealth_transport::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

use super::policy::SharedPolicy;

#[derive(Debug)]
pub struct FaultTransport {
    self_id: NodeId,
    /// node_id -> its direct (clean) internal listener address.
    routes: Arc<RwLock<HashMap<NodeId, SocketAddr>>>,
    /// node_id -> this observer's FaultProxy address for that target.
    proxies: Arc<RwLock<HashMap<NodeId, SocketAddr>>>,
    policy: SharedPolicy,
}

impl FaultTransport {
    pub fn new(self_id: NodeId, policy: SharedPolicy) -> Self {
        Self {
            self_id,
            routes: Arc::new(RwLock::new(HashMap::new())),
            proxies: Arc::new(RwLock::new(HashMap::new())),
            policy,
        }
    }

    /// Record `node`'s direct internal address (used when the edge is clean).
    pub fn set_route(&self, node: NodeId, addr: SocketAddr) {
        self.routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(node, addr);
    }

    /// Record the FaultProxy address to use for `node` when its edge from this
    /// observer carries a wire fault.
    pub fn set_proxy(&self, node: NodeId, addr: SocketAddr) {
        self.proxies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(node, addr);
    }
}

#[async_trait]
impl PeerTransport for FaultTransport {
    fn name(&self) -> &'static str {
        "fault"
    }

    async fn endpoints(&self, peer: &PeerContact, _class: TrafficClass) -> Vec<PeerEndpoint> {
        // Resolve the target socket under the policy lock, then release it
        // before constructing the endpoint. partition/peer-down => empty list,
        // which the caller treats exactly like "no usable address".
        let target = {
            let pol = self
                .policy
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !pol.reachable(self.self_id, peer.node_id) {
                return vec![];
            }
            let has_wire = pol.wire_fault(self.self_id, peer.node_id).is_some();
            if has_wire {
                self.proxies
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&peer.node_id)
                    .copied()
            } else {
                self.routes
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&peer.node_id)
                    .copied()
            }
        };
        match target {
            Some(addr) => vec![PeerEndpoint {
                base_url: format!("http://{addr}"),
                label: format!("fault:{}->{}", self.self_id, peer.node_id),
            }],
            None => vec![],
        }
    }
}
