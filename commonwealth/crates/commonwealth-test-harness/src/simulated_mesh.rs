use std::collections::HashMap;

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::latency::{LatencyMatrix, LatencyRecord};
use commonwealth_core::mesh::Mesh;

use crate::simulated_node::{SimulatedNode, SimulatedNodeBuilder};

/// A simulated mesh of multiple in-process nodes for integration testing.
pub struct SimulatedMesh {
    pub mesh_state: Mesh,
    pub nodes: Vec<SimulatedNode>,
    pub latency_matrix: LatencyMatrix,
}

impl SimulatedMesh {
    /// Create a new empty mesh.
    pub fn new(name: &str) -> Self {
        let mesh = Mesh {
            id: MeshId::from_u128(1),
            name: name.into(),
            join_key_hash: [0u8; 32],
            members: HashMap::new(),
            peers: vec![],
        };
        Self {
            mesh_state: mesh,
            nodes: Vec::new(),
            latency_matrix: LatencyMatrix::new(),
        }
    }

    /// Add a node to the mesh using a builder.
    pub fn add_node(&mut self, builder: SimulatedNodeBuilder) -> usize {
        let node = builder.build_and_register(&mut self.mesh_state);
        let idx = self.nodes.len();
        self.nodes.push(node);
        idx
    }

    /// Set latency between two nodes.
    pub fn set_latency(&mut self, a_idx: usize, b_idx: usize, rtt_ms: f32) {
        let a_id = self.nodes[a_idx].node_id;
        let b_id = self.nodes[b_idx].node_id;
        self.latency_matrix.record(
            a_id,
            b_id,
            LatencyRecord {
                rtt_ms,
                jitter_ms: rtt_ms * 0.1,
                bandwidth_estimate_mbps: 1000.0,
                last_measured: 0,
            },
        );
    }

    /// Set uniform LAN latency between all node pairs.
    pub fn set_lan_latency(&mut self, rtt_ms: f32) {
        let node_ids: Vec<NodeId> = self.nodes.iter().map(|n| n.node_id).collect();
        for (i, &a) in node_ids.iter().enumerate() {
            for &b in &node_ids[i + 1..] {
                self.latency_matrix.record(
                    a,
                    b,
                    LatencyRecord {
                        rtt_ms,
                        jitter_ms: rtt_ms * 0.1,
                        bandwidth_estimate_mbps: 1000.0,
                        last_measured: 0,
                    },
                );
            }
        }
    }

    /// Start all node servers. Returns vec of (client_addr, internal_addr).
    pub async fn start_all(&mut self) -> Vec<(std::net::SocketAddr, std::net::SocketAddr)> {
        let mut addrs = Vec::new();
        for node in &mut self.nodes {
            let addr = node.start_servers().await;
            addrs.push(addr);
        }
        addrs
    }

    /// Update all nodes' mesh state to the current mesh_state.
    pub async fn sync_mesh_state(&self) {
        for node in &self.nodes {
            *node.state.inner.mesh.write().await = self.mesh_state.clone();
        }
    }

    /// Get node capabilities as a HashMap (for scheduler input).
    pub fn node_capabilities(
        &self,
    ) -> HashMap<NodeId, commonwealth_core::capabilities::NodeCapabilities> {
        self.nodes
            .iter()
            .map(|n| (n.node_id, n.capabilities()))
            .collect()
    }

    /// Get node IDs.
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.nodes.iter().map(|n| n.node_id).collect()
    }

    /// Shutdown all nodes.
    pub fn shutdown_all(&mut self) {
        for node in &mut self.nodes {
            node.shutdown();
        }
    }
}

impl Drop for SimulatedMesh {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}
