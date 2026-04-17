use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, warn};

use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;

use crate::gossip::{
    gossip_exchange, select_gossip_peers, GossipEntry, GossipKey, GossipState, GossipValue,
};

/// Configuration for the gossip service.
#[derive(Debug, Clone)]
pub struct GossipServiceConfig {
    /// Interval between gossip rounds. Default: 10 seconds.
    pub gossip_interval: Duration,
    /// Number of random peers to contact per round. Default: 2.
    pub peers_per_round: usize,
}

impl Default for GossipServiceConfig {
    fn default() -> Self {
        Self {
            gossip_interval: Duration::from_secs(10),
            peers_per_round: 2,
        }
    }
}

/// Event emitted when gossip produces a state change.
#[derive(Debug, Clone)]
pub enum GossipEvent {
    /// A peer's state was updated via gossip.
    PeerStateUpdated { node_id: NodeId },
    /// A new entry was learned via gossip.
    NewEntryLearned { key: GossipKey },
}

/// The gossip service manages periodic gossip rounds and state synchronization.
///
/// In production, each gossip round involves network I/O to exchange messages
/// with peers. This struct provides the core logic; the network transport is
/// pluggable via the `GossipTransport` trait.
pub struct GossipService {
    pub state: Arc<RwLock<GossipState>>,
    self_id: NodeId,
    config: GossipServiceConfig,
}

impl GossipService {
    pub fn new(self_id: NodeId, config: GossipServiceConfig) -> Self {
        Self {
            state: Arc::new(RwLock::new(GossipState::new())),
            self_id,
            config,
        }
    }

    /// Publish a local state update to the gossip state.
    pub async fn publish(&self, entry: GossipEntry) {
        let mut state = self.state.write().await;
        state.merge_entry(entry);
    }

    /// Publish this node's capabilities.
    pub async fn publish_capabilities(&self, capabilities: NodeCapabilities) {
        let entry = GossipEntry {
            key: GossipKey::MemberState {
                node_id: self.self_id,
            },
            value: GossipValue::MemberState {
                status: NodeStatus::Online,
                capabilities: Box::new(capabilities.clone()),
            },
            timestamp: capabilities.reported_at,
            origin: self.self_id,
        };
        self.publish(entry).await;
    }

    /// Run a single gossip round against in-memory peer states.
    /// This is used for testing; production uses network transport.
    pub async fn gossip_round_local(
        &self,
        peer_states: &mut [(NodeId, GossipState)],
    ) -> Vec<GossipEvent> {
        let peer_ids: Vec<NodeId> = peer_states.iter().map(|(id, _)| *id).collect();
        let selected = select_gossip_peers(self.self_id, &peer_ids, self.config.peers_per_round);

        let mut events = Vec::new();

        for peer_id in selected {
            let peer = peer_states.iter_mut().find(|(id, _)| *id == peer_id);
            if let Some((_, peer_state)) = peer {
                let mut our_state = self.state.write().await;
                match gossip_exchange(&mut our_state, peer_state) {
                    Ok((our_updates, _peer_updates)) => {
                        if our_updates > 0 {
                            debug!(
                                peer = %peer_id,
                                updates = our_updates,
                                "gossip round: received updates"
                            );
                            events.push(GossipEvent::PeerStateUpdated { node_id: peer_id });
                        }
                    }
                    Err(e) => {
                        warn!(peer = %peer_id, error = %e, "gossip exchange failed");
                    }
                }
            }
        }

        events
    }

    /// Get a snapshot of the current gossip state.
    pub async fn snapshot(&self) -> GossipState {
        self.state.read().await.clone()
    }

    /// Get this node's ID.
    pub fn self_id(&self) -> NodeId {
        self.self_id
    }

    /// Get the gossip interval.
    pub fn gossip_interval(&self) -> Duration {
        self.config.gossip_interval
    }
}

/// Trait for pluggable gossip network transport.
/// Implementations handle the actual network I/O for gossip exchanges.
pub trait GossipTransport: Send + Sync {
    /// Send a gossip exchange to a peer and get back the resulting updates.
    fn exchange_with_peer(
        &self,
        peer_addr: std::net::SocketAddr,
        our_state: &GossipState,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = commonwealth_core::Result<Vec<GossipEntry>>>
                + Send
                + '_,
        >,
    >;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gossip::{GossipEntry, GossipKey, GossipValue};
    use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};

    fn make_caps(reported_at: u64) -> NodeCapabilities {
        NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 32,
                cpu_cores: 8,
                total_storage_gb: 500,
                free_storage_gb: 200,
                network_bandwidth_mbps: Some(1000),
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
        }
    }

    #[tokio::test]
    async fn gossip_service_publish_and_snapshot() {
        let node_id = NodeId::from_u128(1);
        let service = GossipService::new(node_id, GossipServiceConfig::default());

        service.publish_capabilities(make_caps(100)).await;

        let snap = service.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert!(snap.get(&GossipKey::MemberState { node_id }).is_some());
    }

    #[tokio::test]
    async fn gossip_service_local_round() {
        let id_a = NodeId::from_u128(1);
        let id_b = NodeId::from_u128(2);

        let service_a = GossipService::new(id_a, GossipServiceConfig::default());
        service_a.publish_capabilities(make_caps(100)).await;

        // Peer B has its own state.
        let mut peer_b_state = GossipState::new();
        peer_b_state.merge_entry(GossipEntry {
            key: GossipKey::MemberState { node_id: id_b },
            value: GossipValue::MemberState {
                status: NodeStatus::Online,
                capabilities: Box::new(make_caps(200)),
            },
            timestamp: 200,
            origin: id_b,
        });

        let mut peers = vec![(id_b, peer_b_state)];

        let events = service_a.gossip_round_local(&mut peers).await;

        // A should have learned about B.
        let snap = service_a.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert!(!events.is_empty());

        // B should have learned about A.
        assert_eq!(peers[0].1.len(), 2);
    }
}
