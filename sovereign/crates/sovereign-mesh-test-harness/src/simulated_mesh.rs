// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Router;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::latency::{LatencyMatrix, LatencyRecord};
use commonwealth_core::mesh::Mesh;

use crate::simulated_node::{SimulatedNode, SimulatedNodeBuilder};

/// A simulated mesh of multiple in-process nodes for integration testing.
///
/// Generic over the node state for the same reason [`SimulatedNode`] is: the
/// harness meets the runtime at the OICP/contracts seam, so the caller binds
/// the concrete state and supplies the routers.
pub struct SimulatedMesh<S> {
    pub mesh_state: Mesh,
    pub nodes: Vec<SimulatedNode<S>>,
    pub latency_matrix: LatencyMatrix,
}

impl<S> SimulatedMesh<S> {
    /// Create a new empty mesh.
    pub fn new(name: &str) -> Self {
        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: name.into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        };
        Self {
            mesh_state: mesh,
            nodes: Vec::new(),
            latency_matrix: LatencyMatrix::new(),
        }
    }

    /// Add a node to the mesh using a builder, creating its state with
    /// `make_state`.
    pub fn add_node(
        &mut self,
        builder: SimulatedNodeBuilder,
        make_state: impl Fn(NodeId, Mesh) -> S,
    ) -> usize {
        let node = builder.build_and_register(&mut self.mesh_state, &make_state);
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
    ///
    /// The caller supplies each node's two routers, built from its own state.
    pub async fn start_all(
        &mut self,
        make_routers: impl Fn(&S) -> (Router, Router),
    ) -> Vec<(SocketAddr, SocketAddr)> {
        let mut addrs = Vec::new();
        for node in &mut self.nodes {
            let (client_app, internal_app) = make_routers(&node.state);
            let addr = node.start_servers(client_app, internal_app).await;
            addrs.push(addr);
        }
        addrs
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

/// Builder for the twenty-node hacker collective demo scenario.
///
/// Creates a realistic mesh of Apple Silicon MacBooks:
/// - 12 x M3 Pro 36GB
/// - 5 x M3 Pro 18GB
/// - 2 x M3 Max 48GB
/// - 1 x M3 Max 96GB
pub fn twenty_node_hacker_collective<S>(
    make_state: impl Fn(NodeId, Mesh) -> S,
) -> SimulatedMesh<S> {
    let mut mesh = SimulatedMesh::new("hacker-collective");

    // 12 x M3 Pro 36GB — the core workhorses.
    for i in 0..12 {
        let node = SimulatedNodeBuilder::new(100 + i, &format!("m3pro-36-{i}"))
            .gpu(
                "Apple M3 Pro",
                36,
                commonwealth_core::capabilities::ComputeType::Metal,
            )
            .ram_gb(36);
        mesh.add_node(node, &make_state);
    }

    // 5 x M3 Pro 18GB — smaller machines.
    for i in 0..5 {
        let node = SimulatedNodeBuilder::new(200 + i, &format!("m3pro-18-{i}"))
            .gpu(
                "Apple M3 Pro",
                18,
                commonwealth_core::capabilities::ComputeType::Metal,
            )
            .ram_gb(18);
        mesh.add_node(node, &make_state);
    }

    // 2 x M3 Max 48GB.
    for i in 0..2 {
        let node = SimulatedNodeBuilder::new(300 + i, &format!("m3max-48-{i}"))
            .gpu(
                "Apple M3 Max",
                48,
                commonwealth_core::capabilities::ComputeType::Metal,
            )
            .ram_gb(48);
        mesh.add_node(node, &make_state);
    }

    // 1 x M3 Max 96GB.
    let node = SimulatedNodeBuilder::new(400, "m3max-96")
        .gpu(
            "Apple M3 Max",
            96,
            commonwealth_core::capabilities::ComputeType::Metal,
        )
        .ram_gb(96);
    mesh.add_node(node, &make_state);

    // Set uniform LAN latency.
    mesh.set_lan_latency(2.0);

    mesh
}

impl<S> Drop for SimulatedMesh<S> {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}
