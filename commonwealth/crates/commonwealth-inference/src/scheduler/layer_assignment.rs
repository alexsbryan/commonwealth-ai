// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;

use tracing::debug;

use crate::inference_plan::{LayerRange, ShardAssignment};
use commonwealth_core::ids::NodeId;
use commonwealth_core::latency::LatencyMatrix;

/// A node that is eligible to participate in a shard plan.
#[derive(Debug, Clone)]
pub struct EligibleNode {
    pub node_id: NodeId,
    /// Free VRAM available for model hosting (after reserves).
    pub available_vram_gb: f32,
    /// GPU index to use on this node.
    pub gpu_index: u32,
    /// Address for RPC communication.
    pub rpc_address: SocketAddr,
}

/// Result of the layer assignment algorithm.
#[derive(Debug, Clone)]
pub struct LayerAssignmentResult {
    pub assignments: Vec<ShardAssignment>,
    pub entry_node: NodeId,
}

/// Assign model layers across eligible nodes.
///
/// Implements the four principles from the architecture:
/// 1. **Proportional allocation** — more VRAM gets more layers
/// 2. **Contiguous assignment** — each node gets a contiguous range
/// 3. **Topology-aware ordering** — adjacent ranges on low-latency node pairs
/// 4. **Privacy-aware entry node** — prefer `preferred_entry` as layer 0 host
pub fn assign_layers(
    total_layers: u32,
    model_size_bytes: u64,
    nodes: &[EligibleNode],
    latency_matrix: &LatencyMatrix,
    preferred_entry: Option<NodeId>,
) -> Result<LayerAssignmentResult, AssignmentError> {
    if nodes.is_empty() {
        return Err(AssignmentError::NoEligibleNodes);
    }
    if total_layers == 0 {
        return Err(AssignmentError::InvalidModel("zero layers".into()));
    }

    // Filter to nodes with enough VRAM to host at least one layer.
    let bytes_per_layer = model_size_bytes / total_layers as u64;
    let gb_per_layer = bytes_per_layer as f32 / 1_073_741_824.0;

    let eligible: Vec<&EligibleNode> = nodes
        .iter()
        .filter(|n| n.available_vram_gb >= gb_per_layer)
        .collect();

    if eligible.is_empty() {
        return Err(AssignmentError::InsufficientVram {
            needed_gb: gb_per_layer,
            max_available_gb: nodes
                .iter()
                .map(|n| n.available_vram_gb)
                .fold(0.0f32, f32::max),
        });
    }

    // Step 1: Order nodes for topology-aware contiguous assignment.
    let ordered = order_by_topology(&eligible, latency_matrix, preferred_entry);

    // Step 2: Proportional allocation.
    let total_vram: f32 = ordered.iter().map(|n| n.available_vram_gb).sum();
    let layer_counts = allocate_layers_proportional(total_layers, &ordered, total_vram);

    // Step 3: Build contiguous assignments.
    let mut assignments = Vec::new();
    let mut current_layer: u32 = 0;

    for (node, &layer_count) in ordered.iter().zip(layer_counts.iter()) {
        if layer_count == 0 {
            continue;
        }
        let range = LayerRange::new(current_layer, current_layer + layer_count);
        assignments.push(ShardAssignment {
            node_id: node.node_id,
            layers: range,
            gpu_index: node.gpu_index,
            rpc_address: node.rpc_address,
        });
        current_layer += layer_count;
    }

    // Entry node is whoever got layer 0.
    let entry_node = assignments
        .first()
        .map(|a| a.node_id)
        .ok_or(AssignmentError::NoEligibleNodes)?;

    debug!(
        entry_node = %entry_node,
        num_nodes = assignments.len(),
        total_layers,
        "layer assignment complete"
    );

    Ok(LayerAssignmentResult {
        assignments,
        entry_node,
    })
}

/// Allocate layers proportional to VRAM. Guarantees all layers are assigned
/// and each node with allocation gets at least 1 layer.
fn allocate_layers_proportional(
    total_layers: u32,
    nodes: &[&EligibleNode],
    total_vram: f32,
) -> Vec<u32> {
    if total_vram <= 0.0 || nodes.is_empty() {
        return vec![0; nodes.len()];
    }

    // Initial proportional allocation (floor).
    let mut counts: Vec<u32> = nodes
        .iter()
        .map(|n| {
            let fraction = n.available_vram_gb / total_vram;
            (fraction * total_layers as f32).floor() as u32
        })
        .collect();

    // Ensure every node gets at least 1 if we have enough layers.
    for count in counts.iter_mut() {
        if *count == 0 {
            *count = 1;
        }
    }

    // Distribute remaining layers to nodes with the most leftover VRAM fraction.
    let assigned: u32 = counts.iter().sum();
    if assigned < total_layers {
        let remaining = total_layers - assigned;

        // Compute residuals: how much "fractional layer" each node deserves.
        let mut residuals: Vec<(usize, f32)> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let ideal = (n.available_vram_gb / total_vram) * total_layers as f32;
                (i, ideal - counts[i] as f32)
            })
            .collect();
        residuals.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        for &(idx, _) in residuals.iter().take(remaining as usize) {
            counts[idx] += 1;
        }
    } else if assigned > total_layers {
        // Over-allocated (rare, from the minimum-1 rule). Trim from smallest.
        let mut excess = assigned - total_layers;
        let mut sorted_indices: Vec<usize> = (0..counts.len()).collect();
        sorted_indices.sort_by(|&a, &b| counts[a].cmp(&counts[b]));

        for &idx in sorted_indices.iter().rev() {
            if excess == 0 {
                break;
            }
            if counts[idx] > 1 {
                let reduce = (counts[idx] - 1).min(excess);
                counts[idx] -= reduce;
                excess -= reduce;
            }
        }
    }

    counts
}

/// Order nodes for topology-aware assignment.
///
/// The goal: adjacent layer ranges should be on low-latency node pairs.
/// Strategy: greedy nearest-neighbor traversal starting from the preferred
/// entry node (or the node with the most VRAM if no preference).
fn order_by_topology<'a>(
    nodes: &[&'a EligibleNode],
    latency_matrix: &LatencyMatrix,
    preferred_entry: Option<NodeId>,
) -> Vec<&'a EligibleNode> {
    if nodes.len() <= 1 {
        return nodes.to_vec();
    }

    // Pick start node.
    let start_idx = if let Some(pref) = preferred_entry {
        nodes
            .iter()
            .position(|n| n.node_id == pref)
            .unwrap_or_else(|| {
                // Preferred entry not eligible; pick highest VRAM.
                nodes
                    .iter()
                    .enumerate()
                    .max_by(|a, b| {
                        a.1.available_vram_gb
                            .partial_cmp(&b.1.available_vram_gb)
                            .unwrap()
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            })
    } else {
        // No preference: pick highest VRAM.
        nodes
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.available_vram_gb
                    .partial_cmp(&b.1.available_vram_gb)
                    .unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    };

    // Greedy nearest-neighbor traversal.
    let mut result = Vec::with_capacity(nodes.len());
    let mut visited = vec![false; nodes.len()];

    visited[start_idx] = true;
    result.push(nodes[start_idx]);

    for _ in 1..nodes.len() {
        let current = result.last().unwrap();

        // Find the closest unvisited node.
        let mut best_idx = None;
        let mut best_rtt = f32::MAX;

        for (j, node) in nodes.iter().enumerate() {
            if visited[j] {
                continue;
            }
            let rtt = latency_matrix
                .get(current.node_id, node.node_id)
                .map(|r| r.rtt_ms)
                .unwrap_or(100.0); // Default: assume 100ms if unknown.
            if rtt < best_rtt {
                best_rtt = rtt;
                best_idx = Some(j);
            }
        }

        if let Some(idx) = best_idx {
            visited[idx] = true;
            result.push(nodes[idx]);
        }
    }

    result
}

/// Errors from the layer assignment algorithm.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AssignmentError {
    #[error("no eligible nodes for layer assignment")]
    NoEligibleNodes,

    #[error("insufficient VRAM: need {needed_gb:.1} GB per layer, max available is {max_available_gb:.1} GB")]
    InsufficientVram {
        needed_gb: f32,
        max_available_gb: f32,
    },

    #[error("invalid model: {0}")]
    InvalidModel(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::latency::{LatencyMatrix, LatencyRecord};

    fn node(id: u128, vram: f32) -> EligibleNode {
        EligibleNode {
            node_id: NodeId::from_u128(id),
            available_vram_gb: vram,
            gpu_index: 0,
            rpc_address: format!("127.0.0.1:{}", 50000 + id as u16).parse().unwrap(),
        }
    }

    fn latency_record(rtt: f32) -> LatencyRecord {
        LatencyRecord {
            rtt_ms: rtt,
            jitter_ms: 0.1,
            bandwidth_estimate_mbps: 1000.0,
            last_measured: 0,
        }
    }

    // -- Proportional allocation tests --

    #[test]
    fn single_node_gets_all_layers() {
        let nodes = vec![node(1, 24.0)];
        let matrix = LatencyMatrix::new();
        let result = assign_layers(64, 17_000_000_000, &nodes, &matrix, None).unwrap();

        assert_eq!(result.assignments.len(), 1);
        assert_eq!(result.assignments[0].layers, LayerRange::new(0, 64));
        assert_eq!(result.entry_node, NodeId::from_u128(1));
    }

    #[test]
    fn two_equal_nodes_split_evenly() {
        let nodes = vec![node(1, 12.0), node(2, 12.0)];
        let matrix = LatencyMatrix::new();
        let result = assign_layers(64, 17_000_000_000, &nodes, &matrix, None).unwrap();

        assert_eq!(result.assignments.len(), 2);
        let total: u32 = result.assignments.iter().map(|a| a.layers.count()).sum();
        assert_eq!(total, 64);
        // Each should get ~32.
        assert_eq!(result.assignments[0].layers.count(), 32);
        assert_eq!(result.assignments[1].layers.count(), 32);
    }

    #[test]
    fn proportional_allocation_asymmetric() {
        // Alice: 24 GB, Bob: 8 GB. Total 32 GB, 64 layers.
        // Alice should get ~48 layers (75%), Bob ~16 (25%).
        let nodes = vec![node(1, 24.0), node(2, 8.0)];
        let matrix = LatencyMatrix::new();
        let result = assign_layers(64, 17_000_000_000, &nodes, &matrix, None).unwrap();

        assert_eq!(result.assignments.len(), 2);
        let total: u32 = result.assignments.iter().map(|a| a.layers.count()).sum();
        assert_eq!(total, 64);

        // Find Alice's assignment (highest VRAM gets first in ordering).
        let alice_layers = result
            .assignments
            .iter()
            .find(|a| a.node_id == NodeId::from_u128(1))
            .unwrap()
            .layers
            .count();
        let bob_layers = result
            .assignments
            .iter()
            .find(|a| a.node_id == NodeId::from_u128(2))
            .unwrap()
            .layers
            .count();

        assert!(
            alice_layers > bob_layers,
            "Alice ({alice_layers}) should get more than Bob ({bob_layers})"
        );
        assert!(
            (44..=52).contains(&alice_layers),
            "Alice should get ~48, got {alice_layers}"
        );
    }

    #[test]
    fn contiguous_ranges() {
        let nodes = vec![node(1, 24.0), node(2, 12.0), node(3, 8.0)];
        let matrix = LatencyMatrix::new();
        let result = assign_layers(64, 17_000_000_000, &nodes, &matrix, None).unwrap();

        // Verify ranges are contiguous and cover all layers.
        let mut expected_start = 0;
        for a in &result.assignments {
            assert_eq!(a.layers.start, expected_start, "gap in layer ranges");
            expected_start = a.layers.end;
        }
        assert_eq!(expected_start, 64, "layers don't cover all 64");
    }

    // -- Privacy-aware entry node tests --

    #[test]
    fn preferred_entry_node_gets_layer_zero() {
        let nodes = vec![node(1, 24.0), node(2, 12.0), node(3, 8.0)];
        let matrix = LatencyMatrix::new();
        let result = assign_layers(
            64,
            17_000_000_000,
            &nodes,
            &matrix,
            Some(NodeId::from_u128(2)),
        )
        .unwrap();

        // Node 2 should be first (layer 0) even though it has less VRAM.
        assert_eq!(result.entry_node, NodeId::from_u128(2));
        assert_eq!(result.assignments[0].node_id, NodeId::from_u128(2));
        assert_eq!(result.assignments[0].layers.start, 0);
    }

    // -- Topology-aware ordering tests --

    #[test]
    fn topology_aware_ordering_prefers_low_latency() {
        let nodes = vec![node(1, 12.0), node(2, 12.0), node(3, 12.0)];
        let mut matrix = LatencyMatrix::new();

        // Node 1→2: 1ms, Node 1→3: 50ms, Node 2→3: 2ms.
        matrix.record(
            NodeId::from_u128(1),
            NodeId::from_u128(2),
            latency_record(1.0),
        );
        matrix.record(
            NodeId::from_u128(1),
            NodeId::from_u128(3),
            latency_record(50.0),
        );
        matrix.record(
            NodeId::from_u128(2),
            NodeId::from_u128(3),
            latency_record(2.0),
        );

        let result = assign_layers(
            64,
            17_000_000_000,
            &nodes,
            &matrix,
            Some(NodeId::from_u128(1)),
        )
        .unwrap();

        // Starting from node 1, nearest is node 2 (1ms), then node 3 (2ms from node 2).
        assert_eq!(result.assignments[0].node_id, NodeId::from_u128(1));
        assert_eq!(result.assignments[1].node_id, NodeId::from_u128(2));
        assert_eq!(result.assignments[2].node_id, NodeId::from_u128(3));
    }

    // -- Error cases --

    #[test]
    fn no_nodes_returns_error() {
        let matrix = LatencyMatrix::new();
        let result = assign_layers(64, 17_000_000_000, &[], &matrix, None);
        assert!(matches!(result, Err(AssignmentError::NoEligibleNodes)));
    }

    #[test]
    fn insufficient_vram_returns_error() {
        // Model needs ~0.27 GB per layer, node only has 0.1 GB.
        let nodes = vec![node(1, 0.1)];
        let matrix = LatencyMatrix::new();
        let result = assign_layers(64, 17_000_000_000, &nodes, &matrix, None);
        assert!(matches!(
            result,
            Err(AssignmentError::InsufficientVram { .. })
        ));
    }

    // -- Five-node scenario from the architecture --

    #[test]
    fn architecture_five_node_scenario() {
        // From the ARCHITECTURE.md scenario table:
        // Alice: Strix Halo 32 GB shared → ~24 GB usable (after reserves)
        // Bob: RTX 4090 24 GB → ~20 GB usable
        // Carol: M3 Ultra 192 GB unified → ~140 GB usable
        // Dave: 2× RTX 3090 48 GB → ~40 GB usable
        // Eve: Integrated 16 GB → too small for layers, excluded

        let nodes = vec![
            node(1, 24.0),  // Alice
            node(2, 20.0),  // Bob
            node(3, 140.0), // Carol
            node(4, 40.0),  // Dave
            node(5, 2.0),   // Eve — small but might get 1 layer
        ];

        let mut matrix = LatencyMatrix::new();
        // All on the same LAN: ~1ms between all pairs.
        for i in 1..=5u128 {
            for j in (i + 1)..=5 {
                matrix.record(
                    NodeId::from_u128(i),
                    NodeId::from_u128(j),
                    latency_record(1.0),
                );
            }
        }

        // 70B model, ~80 layers, ~40 GB.
        let result = assign_layers(80, 40_000_000_000, &nodes, &matrix, None).unwrap();

        let total: u32 = result.assignments.iter().map(|a| a.layers.count()).sum();
        assert_eq!(total, 80);

        // Carol (140 GB) should have the most layers.
        let carol_layers = result
            .assignments
            .iter()
            .find(|a| a.node_id == NodeId::from_u128(3))
            .unwrap()
            .layers
            .count();
        assert!(
            carol_layers > 30,
            "Carol should have most layers, got {carol_layers}"
        );

        // All ranges should be contiguous.
        let mut expected = 0;
        for a in &result.assignments {
            assert_eq!(a.layers.start, expected);
            expected = a.layers.end;
        }
        assert_eq!(expected, 80);
    }

    // -- Proportional allocation unit tests --

    #[test]
    fn allocate_layers_proportional_basic() {
        let nodes = [node(1, 24.0), node(2, 8.0)];
        let node_refs: Vec<&EligibleNode> = nodes.iter().collect();
        let counts = allocate_layers_proportional(64, &node_refs, 32.0);
        assert_eq!(counts.iter().sum::<u32>(), 64);
        assert!(counts[0] > counts[1]);
    }

    #[test]
    fn allocate_layers_all_equal() {
        let nodes = [node(1, 10.0), node(2, 10.0), node(3, 10.0)];
        let node_refs: Vec<&EligibleNode> = nodes.iter().collect();
        let counts = allocate_layers_proportional(30, &node_refs, 30.0);
        assert_eq!(counts, vec![10, 10, 10]);
    }

    #[test]
    fn allocate_layers_rounding() {
        // 3 nodes, 10 layers — can't divide evenly.
        let nodes = [node(1, 10.0), node(2, 10.0), node(3, 10.0)];
        let node_refs: Vec<&EligibleNode> = nodes.iter().collect();
        let counts = allocate_layers_proportional(10, &node_refs, 30.0);
        assert_eq!(counts.iter().sum::<u32>(), 10);
        // Each should get 3 or 4.
        for &c in &counts {
            assert!((3..=4).contains(&c), "expected 3-4, got {c}");
        }
    }
}
