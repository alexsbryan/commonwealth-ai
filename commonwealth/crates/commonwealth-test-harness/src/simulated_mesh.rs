use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use commonwealth_app::manifest::MeshAppManifest;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::latency::{LatencyMatrix, LatencyRecord};
use commonwealth_core::mesh::Mesh;
use commonwealth_inference::plan::MeshPlan;
use commonwealth_inference::scheduler::adaptive::{InferenceScheduler, NodeProfile, SchedulerConfig};

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

    // ── Adaptive scheduler helpers ─────────────────────────────────────

    /// Build node profiles for the adaptive scheduler from the current mesh.
    pub fn scheduler_profiles(&self) -> HashMap<NodeId, NodeProfile> {
        self.nodes
            .iter()
            .map(|n| {
                // Use system RAM as the memory metric (Apple Silicon unified memory).
                let memory_gb = n.hardware.system_ram_gb;
                // Collect model IDs from the node's registered models in the store.
                let model_ids = n
                    .state
                    .inner
                    .inference_store
                    .list_models()
                    .values()
                    .map(|m| m.name.clone())
                    .collect();
                (
                    n.node_id,
                    NodeProfile {
                        available_memory_gb: memory_gb,
                        model_ids,
                    },
                )
            })
            .collect()
    }

    /// Create an InferenceScheduler configured for the leader node in this mesh.
    pub fn make_scheduler(&self) -> InferenceScheduler {
        let leader = self
            .nodes
            .iter()
            .map(|n| n.node_id)
            .min()
            .expect("mesh has no nodes");
        let mut scheduler = InferenceScheduler::new(leader, SchedulerConfig::default());
        // Set online nodes so the scheduler knows it's the leader.
        scheduler.online_nodes = self.node_ids();
        scheduler
    }

    /// Create an InferenceScheduler with a custom config.
    pub fn make_scheduler_with_config(&self, config: SchedulerConfig) -> InferenceScheduler {
        let leader = self
            .nodes
            .iter()
            .map(|n| n.node_id)
            .min()
            .expect("mesh has no nodes");
        let mut scheduler = InferenceScheduler::new(leader, config);
        scheduler.online_nodes = self.node_ids();
        scheduler
    }

    /// Write a MeshPlan to all nodes' stores (simulating gossip propagation).
    pub fn propagate_mesh_plan(&self, plan: &MeshPlan) {
        for node in &self.nodes {
            node.state.inner.inference_store.set_mesh_plan(plan);
        }
    }

    /// Read the current MeshPlan from the first node (all should agree after propagation).
    pub fn current_mesh_plan(&self) -> Option<MeshPlan> {
        self.nodes
            .first()
            .and_then(|n| n.state.inner.inference_store.get_mesh_plan())
    }

    /// Check if all nodes have the same MeshPlan version.
    pub fn plans_converged(&self) -> bool {
        let versions: Vec<Option<u64>> = self
            .nodes
            .iter()
            .map(|n| n.state.inner.inference_store.get_mesh_plan().map(|p| p.version))
            .collect();

        if versions.is_empty() {
            return false;
        }

        let first = versions[0];
        first.is_some() && versions.iter().all(|v| *v == first)
    }
}

/// Builder for the twenty-node hacker collective demo scenario.
///
/// Creates a realistic mesh of Apple Silicon MacBooks:
/// - 12 x M3 Pro 36GB
/// - 5 x M3 Pro 18GB
/// - 2 x M3 Max 48GB
/// - 1 x M3 Max 96GB
pub fn twenty_node_hacker_collective() -> SimulatedMesh {
    let mut mesh = SimulatedMesh::new("hacker-collective");

    // 12 x M3 Pro 36GB — the core workhorses.
    for i in 0..12 {
        let node = SimulatedNodeBuilder::new(100 + i, &format!("m3pro-36-{i}"))
            .gpu("Apple M3 Pro", 36, commonwealth_core::capabilities::ComputeType::Metal)
            .ram_gb(36);
        mesh.add_node(node);
    }

    // 5 x M3 Pro 18GB — smaller machines.
    for i in 0..5 {
        let node = SimulatedNodeBuilder::new(200 + i, &format!("m3pro-18-{i}"))
            .gpu("Apple M3 Pro", 18, commonwealth_core::capabilities::ComputeType::Metal)
            .ram_gb(18);
        mesh.add_node(node);
    }

    // 2 x M3 Max 48GB.
    for i in 0..2 {
        let node = SimulatedNodeBuilder::new(300 + i, &format!("m3max-48-{i}"))
            .gpu("Apple M3 Max", 48, commonwealth_core::capabilities::ComputeType::Metal)
            .ram_gb(48);
        mesh.add_node(node);
    }

    // 1 x M3 Max 96GB.
    let node = SimulatedNodeBuilder::new(400, "m3max-96")
        .gpu("Apple M3 Max", 96, commonwealth_core::capabilities::ComputeType::Metal)
        .ram_gb(96);
    mesh.add_node(node);

    // Set uniform LAN latency.
    mesh.set_lan_latency(2.0);

    mesh
}

impl Drop for SimulatedMesh {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}
