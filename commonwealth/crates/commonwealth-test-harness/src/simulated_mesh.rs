use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use commonwealth_app::manifest::MeshAppManifest;
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

    // ── Platform helpers ──────────────────────────────────────────────────────

    /// Register a mock app manifest on the given node's AppRegistry.
    pub async fn register_mock_app(&mut self, node_idx: usize, manifest: MeshAppManifest) {
        self.nodes[node_idx]
            .state
            .inner
            .app_registry
            .register(manifest)
            .await;
    }

    /// Write a value into the MeshStore on the given node.
    pub fn store_set(
        &self,
        node_idx: usize,
        app_id: &str,
        key: &str,
        value: &[u8],
    ) {
        let origin = self.nodes[node_idx].node_id;
        self.nodes[node_idx]
            .state
            .inner
            .mesh_store
            .set(app_id, key, Bytes::copy_from_slice(value), origin)
            .expect("store_set failed");
    }

    /// Read a value from the MeshStore on the given node.
    pub fn store_get(&self, node_idx: usize, app_id: &str, key: &str) -> Option<Vec<u8>> {
        self.nodes[node_idx]
            .state
            .inner
            .mesh_store
            .get(app_id, key)
            .expect("store_get failed")
            .map(|e| e.value.to_vec())
    }

    /// Poll until every node has the expected key/value, or until timeout expires.
    ///
    /// Returns `true` if all nodes converged, `false` if timed out.
    pub async fn wait_store_converged(
        &self,
        app_id: &str,
        key: &str,
        expected: &[u8],
        timeout: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let all_match = self.nodes.iter().all(|node| {
                node.state
                    .inner
                    .mesh_store
                    .get(app_id, key)
                    .ok()
                    .flatten()
                    .map(|e| e.value.as_ref() == expected)
                    .unwrap_or(false)
            });
            if all_match {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for SimulatedMesh {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}
