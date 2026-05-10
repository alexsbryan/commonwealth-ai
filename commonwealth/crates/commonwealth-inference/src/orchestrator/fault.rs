use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::debug;

use commonwealth_core::ids::NodeId;

/// Configuration for the fault detector.
#[derive(Debug, Clone)]
pub struct FaultDetectorConfig {
    /// How long before a node is considered "suspected". Default: 15 seconds.
    pub suspected_timeout: Duration,
    /// How long before a node is considered "away". Default: 30 seconds.
    pub away_timeout: Duration,
    /// How long before an "away" node is declared failed. Default: 120 seconds.
    pub failure_timeout: Duration,
}

impl Default for FaultDetectorConfig {
    fn default() -> Self {
        Self {
            suspected_timeout: Duration::from_secs(15),
            away_timeout: Duration::from_secs(30),
            failure_timeout: Duration::from_secs(120),
        }
    }
}

/// Current fault status of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultStatus {
    Healthy,
    Suspected,
    Away,
    Failed,
    Departing,
    Departed,
}

/// Tracked state for a single node.
#[derive(Debug, Clone)]
pub struct NodeFaultState {
    pub node_id: NodeId,
    pub last_heartbeat: Instant,
    pub status: FaultStatus,
    pub consecutive_failures: u32,
}

/// Events emitted when a node's fault status changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultEvent {
    NodeSuspected { node_id: NodeId },
    NodeAway { node_id: NodeId },
    NodeFailed { node_id: NodeId },
    NodeDeparting { node_id: NodeId },
    NodeDeparted { node_id: NodeId },
    NodeRecovered { node_id: NodeId },
}

/// Tracks node health from the mesh perspective.
pub struct FaultDetector {
    nodes: HashMap<NodeId, NodeFaultState>,
    config: FaultDetectorConfig,
}

impl FaultDetector {
    pub fn new(config: FaultDetectorConfig) -> Self {
        Self {
            nodes: HashMap::new(),
            config,
        }
    }

    /// Start tracking a node.
    pub fn register_node(&mut self, node_id: NodeId) {
        self.nodes.insert(
            node_id,
            NodeFaultState {
                node_id,
                last_heartbeat: Instant::now(),
                status: FaultStatus::Healthy,
                consecutive_failures: 0,
            },
        );
    }

    /// Stop tracking a node.
    pub fn remove_node(&mut self, node_id: NodeId) {
        self.nodes.remove(&node_id);
    }

    /// Record a heartbeat from a node. Returns a recovery event if the node
    /// was previously in a non-healthy state.
    pub fn record_heartbeat(&mut self, node_id: NodeId) -> Option<FaultEvent> {
        if let Some(state) = self.nodes.get_mut(&node_id) {
            let was_unhealthy = state.status != FaultStatus::Healthy;
            state.last_heartbeat = Instant::now();
            state.consecutive_failures = 0;

            if was_unhealthy
                && state.status != FaultStatus::Departing
                && state.status != FaultStatus::Departed
            {
                state.status = FaultStatus::Healthy;
                debug!(node = %node_id, "node recovered");
                return Some(FaultEvent::NodeRecovered { node_id });
            }
            state.status = FaultStatus::Healthy;
        }
        None
    }

    /// Check all tracked nodes against timeouts. Returns events for any transitions.
    pub fn check_all(&mut self) -> Vec<FaultEvent> {
        let now = Instant::now();
        let mut events = Vec::new();

        for state in self.nodes.values_mut() {
            // Skip nodes that are departing/departed — they're handled separately.
            if state.status == FaultStatus::Departing || state.status == FaultStatus::Departed {
                continue;
            }

            let elapsed = now.duration_since(state.last_heartbeat);
            let old_status = state.status;

            let new_status = if elapsed >= self.config.failure_timeout {
                FaultStatus::Failed
            } else if elapsed >= self.config.away_timeout {
                FaultStatus::Away
            } else if elapsed >= self.config.suspected_timeout {
                FaultStatus::Suspected
            } else {
                FaultStatus::Healthy
            };

            if new_status != old_status {
                state.status = new_status;
                state.consecutive_failures += 1;

                let event = match new_status {
                    FaultStatus::Suspected => FaultEvent::NodeSuspected {
                        node_id: state.node_id,
                    },
                    FaultStatus::Away => FaultEvent::NodeAway {
                        node_id: state.node_id,
                    },
                    FaultStatus::Failed => FaultEvent::NodeFailed {
                        node_id: state.node_id,
                    },
                    FaultStatus::Healthy => FaultEvent::NodeRecovered {
                        node_id: state.node_id,
                    },
                    _ => continue,
                };
                events.push(event);
            }
        }

        events
    }

    /// Begin graceful departure for a node (30-second countdown starts externally).
    pub fn begin_graceful_departure(&mut self, node_id: NodeId) -> Option<FaultEvent> {
        if let Some(state) = self.nodes.get_mut(&node_id) {
            state.status = FaultStatus::Departing;
            Some(FaultEvent::NodeDeparting { node_id })
        } else {
            None
        }
    }

    /// Mark a node as fully departed.
    pub fn mark_departed(&mut self, node_id: NodeId) -> Option<FaultEvent> {
        if let Some(state) = self.nodes.get_mut(&node_id) {
            state.status = FaultStatus::Departed;
            Some(FaultEvent::NodeDeparted { node_id })
        } else {
            None
        }
    }

    /// Get all node IDs that are in a failed or departed state.
    pub fn failed_nodes(&self) -> Vec<NodeId> {
        self.nodes
            .values()
            .filter(|s| s.status == FaultStatus::Failed || s.status == FaultStatus::Departed)
            .map(|s| s.node_id)
            .collect()
    }

    /// Get all node IDs that are healthy.
    pub fn healthy_nodes(&self) -> Vec<NodeId> {
        self.nodes
            .values()
            .filter(|s| s.status == FaultStatus::Healthy)
            .map(|s| s.node_id)
            .collect()
    }

    /// Get a node's current fault status.
    pub fn node_status(&self, node_id: NodeId) -> Option<FaultStatus> {
        self.nodes.get(&node_id).map(|s| s.status)
    }

    /// Number of tracked nodes.
    pub fn tracked_count(&self) -> usize {
        self.nodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_config() -> FaultDetectorConfig {
        FaultDetectorConfig {
            suspected_timeout: Duration::from_millis(50),
            away_timeout: Duration::from_millis(100),
            failure_timeout: Duration::from_millis(200),
        }
    }

    #[test]
    fn register_and_check_healthy() {
        let mut fd = FaultDetector::new(fast_config());
        let id = NodeId::from_u128(1);
        fd.register_node(id);

        assert_eq!(fd.node_status(id), Some(FaultStatus::Healthy));
        assert_eq!(fd.tracked_count(), 1);
        assert_eq!(fd.healthy_nodes(), vec![id]);
        assert!(fd.failed_nodes().is_empty());
    }

    #[test]
    fn heartbeat_keeps_node_healthy() {
        let mut fd = FaultDetector::new(fast_config());
        let id = NodeId::from_u128(1);
        fd.register_node(id);

        let event = fd.record_heartbeat(id);
        assert!(event.is_none()); // Already healthy, no transition.
        assert_eq!(fd.node_status(id), Some(FaultStatus::Healthy));
    }

    #[test]
    fn timeout_transitions_suspected_away_failed() {
        let mut fd = FaultDetector::new(fast_config());
        let id = NodeId::from_u128(1);
        fd.register_node(id);

        // Backdate the heartbeat to simulate time passing.
        fd.nodes.get_mut(&id).unwrap().last_heartbeat = Instant::now() - Duration::from_millis(60);

        let events = fd.check_all();
        assert_eq!(events, vec![FaultEvent::NodeSuspected { node_id: id }]);
        assert_eq!(fd.node_status(id), Some(FaultStatus::Suspected));

        // More time passes → Away.
        fd.nodes.get_mut(&id).unwrap().last_heartbeat = Instant::now() - Duration::from_millis(110);
        let events = fd.check_all();
        assert_eq!(events, vec![FaultEvent::NodeAway { node_id: id }]);

        // Even more → Failed.
        fd.nodes.get_mut(&id).unwrap().last_heartbeat = Instant::now() - Duration::from_millis(210);
        let events = fd.check_all();
        assert_eq!(events, vec![FaultEvent::NodeFailed { node_id: id }]);
        assert_eq!(fd.failed_nodes(), vec![id]);
    }

    #[test]
    fn heartbeat_recovers_suspected_node() {
        let mut fd = FaultDetector::new(fast_config());
        let id = NodeId::from_u128(1);
        fd.register_node(id);

        // Make it suspected.
        fd.nodes.get_mut(&id).unwrap().last_heartbeat = Instant::now() - Duration::from_millis(60);
        fd.check_all();
        assert_eq!(fd.node_status(id), Some(FaultStatus::Suspected));

        // Heartbeat recovers.
        let event = fd.record_heartbeat(id);
        assert_eq!(event, Some(FaultEvent::NodeRecovered { node_id: id }));
        assert_eq!(fd.node_status(id), Some(FaultStatus::Healthy));
    }

    #[test]
    fn graceful_departure_flow() {
        let mut fd = FaultDetector::new(fast_config());
        let id = NodeId::from_u128(1);
        fd.register_node(id);

        let event = fd.begin_graceful_departure(id);
        assert_eq!(event, Some(FaultEvent::NodeDeparting { node_id: id }));
        assert_eq!(fd.node_status(id), Some(FaultStatus::Departing));

        // Departing nodes are skipped by check_all.
        fd.nodes.get_mut(&id).unwrap().last_heartbeat = Instant::now() - Duration::from_millis(500);
        let events = fd.check_all();
        assert!(events.is_empty());

        let event = fd.mark_departed(id);
        assert_eq!(event, Some(FaultEvent::NodeDeparted { node_id: id }));
        assert!(fd.failed_nodes().contains(&id));
    }

    #[test]
    fn heartbeat_does_not_recover_departing_node() {
        let mut fd = FaultDetector::new(fast_config());
        let id = NodeId::from_u128(1);
        fd.register_node(id);
        fd.begin_graceful_departure(id);

        let event = fd.record_heartbeat(id);
        assert!(event.is_none()); // Departing nodes don't "recover".
    }

    #[test]
    fn multiple_nodes_independent_tracking() {
        let mut fd = FaultDetector::new(fast_config());
        let id1 = NodeId::from_u128(1);
        let id2 = NodeId::from_u128(2);
        fd.register_node(id1);
        fd.register_node(id2);

        // Only node 1 goes stale.
        fd.nodes.get_mut(&id1).unwrap().last_heartbeat = Instant::now() - Duration::from_millis(60);

        let events = fd.check_all();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], FaultEvent::NodeSuspected { node_id: id1 });
        assert_eq!(fd.node_status(id2), Some(FaultStatus::Healthy));
    }

    #[test]
    fn remove_node_stops_tracking() {
        let mut fd = FaultDetector::new(fast_config());
        let id = NodeId::from_u128(1);
        fd.register_node(id);
        assert_eq!(fd.tracked_count(), 1);

        fd.remove_node(id);
        assert_eq!(fd.tracked_count(), 0);
        assert_eq!(fd.node_status(id), None);
    }

    #[test]
    fn no_events_when_all_healthy() {
        let mut fd = FaultDetector::new(fast_config());
        for i in 1..=5 {
            fd.register_node(NodeId::from_u128(i));
        }
        let events = fd.check_all();
        assert!(events.is_empty());
    }
}
