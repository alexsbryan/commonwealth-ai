//! Topology events that trigger scheduler replanning.
//!
//! These events bridge the discovery/gossip layer and the fault detector
//! to the scheduler's event loop. The scheduler consumes a stream of
//! these events and debounces rapid changes before replanning.

use serde::{Deserialize, Serialize};

use commonwealth_core::NodeId;

/// Events from the gossip/discovery layer that trigger replanning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TopologyEvent {
    /// A new node joined the mesh and its capabilities are available.
    NodeJoined(NodeId),
    /// A node departed — either gracefully or detected by the fault detector.
    NodeDeparted(NodeId),
    /// A node's model portfolio changed — a large model became available.
    ModelAvailable { node_id: NodeId, model_id: String },
    /// A node's model was unloaded (memory pressure, manual unload).
    ModelUnavailable { node_id: NodeId, model_id: String },
    /// Leadership changed — the new leader should start the scheduler.
    LeaderChanged(NodeId),
}
