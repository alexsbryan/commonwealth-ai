// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::ids::{ModelId, NodeId};

/// The complete inference plan for the mesh — one shard plan per loaded model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferencePlan {
    pub model_plans: Vec<ShardPlan>,
}

/// How a single model is sharded across mesh nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardPlan {
    pub model: ModelId,
    pub entry_node: NodeId,
    pub assignments: Vec<ShardAssignment>,
    pub estimated_tokens_per_sec: f32,
    pub estimated_ttft_ms: u32,
}

/// A single node's assignment within a shard plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardAssignment {
    pub node_id: NodeId,
    pub layers: LayerRange,
    pub gpu_index: u32,
    pub rpc_address: SocketAddr,
}

/// A contiguous range of model layers assigned to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRange {
    /// First layer (inclusive).
    pub start: u32,
    /// Last layer (exclusive).
    pub end: u32,
}

impl LayerRange {
    pub fn new(start: u32, end: u32) -> Self {
        debug_assert!(start < end, "empty layer range: {start}..{end}");
        Self { start, end }
    }

    pub fn count(&self) -> u32 {
        self.end - self.start
    }
}

/// State machine for graceful model transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTransition {
    pub outgoing: ShardPlan,
    pub incoming: ShardPlan,
    pub state: TransitionState,
}

/// Current phase of a model transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionState {
    /// New model loading. Old model still serving.
    Loading,
    /// New model loaded. Draining old model.
    Ready,
    /// Old model unloaded. Transition complete.
    Complete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_range_count() {
        let r = LayerRange::new(0, 32);
        assert_eq!(r.count(), 32);
    }

    #[test]
    fn layer_range_serde_roundtrip() {
        let r = LayerRange::new(16, 48);
        let json = serde_json::to_string(&r).unwrap();
        let back: LayerRange = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn shard_plan_serde_roundtrip() {
        let plan = ShardPlan {
            model: ModelId::from_u128(1),
            entry_node: NodeId::from_u128(10),
            assignments: vec![ShardAssignment {
                node_id: NodeId::from_u128(10),
                layers: LayerRange::new(0, 32),
                gpu_index: 0,
                rpc_address: "127.0.0.1:50051".parse().unwrap(),
            }],
            estimated_tokens_per_sec: 45.0,
            estimated_ttft_ms: 1100,
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: ShardPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.assignments.len(), 1);
        assert_eq!(back.assignments[0].layers.count(), 32);
    }

    #[test]
    fn transition_state_serde_roundtrip() {
        for state in [
            TransitionState::Loading,
            TransitionState::Ready,
            TransitionState::Complete,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: TransitionState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }
}
