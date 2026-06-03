//! Mesh-level inference plan types.
//!
//! A [`MeshPlan`] describes the complete inference topology for the current
//! mesh moment: which nodes run which models in which role, and how incoming
//! requests are routed between tiers. Computed by the scheduler leader,
//! stored in MeshStore, consumed by every node's orchestrator.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::inference_plan::ShardPlan;
use commonwealth_core::ids::{NodeId, PlanId};

// ─── MeshPlan ────────────────────────────────────────────────

/// The complete inference topology for the current mesh moment.
/// Computed by the scheduler leader, stored in MeshStore,
/// consumed by every node's orchestrator and request router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPlan {
    pub id: PlanId,
    pub computed_at: chrono::DateTime<chrono::Utc>,
    pub trigger: PlanTrigger,
    pub strategy: SchedulingStrategy,
    /// Each node's role(s) in this plan.
    pub node_roles: HashMap<NodeId, Vec<NodeRole>>,
    /// How incoming requests route to tiers.
    pub router: RequestRouter,
    /// Version counter — monotonically increasing.
    /// Nodes reject plans with lower version than the one they hold.
    pub version: u64,
}

// ─── Strategy ────────────────────────────────────────────────

/// The inference topology strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchedulingStrategy {
    /// One instance, potentially sharded across multiple nodes.
    /// Best quality. Latency scales with inter-node bandwidth.
    SingleInstance { shard_plan: ShardPlan },

    /// Independent instances across eligible nodes.
    /// Best throughput. Quality capped by what fits on one node.
    ParallelInstances {
        model_id: String,
        instance_nodes: Vec<NodeId>,
        load_policy: LoadPolicy,
    },

    /// Quality tier (single large model) for hard requests,
    /// throughput tier (parallel smaller model) for everything else.
    /// The router decides per-request which tier handles it.
    Tiered {
        quality: Box<SchedulingStrategy>,
        throughput: Box<SchedulingStrategy>,
        router: TierRouter,
    },

    /// Mesh too small or degraded to run any model.
    /// Requests return 503 with a clear explanation.
    Unavailable { reason: UnavailableReason },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnavailableReason {
    /// No nodes with enough memory for any configured model.
    InsufficientMemory {
        min_required_gb: u32,
        available_gb: u32,
    },
    /// Convergence window — new leader elected, waiting for profiles.
    ConvergenceInProgress {
        started_at: chrono::DateTime<chrono::Utc>,
    },
    /// No models configured.
    NoModels,
}

// ─── Triggers ────────────────────────────────────────────────

/// What caused this plan to be computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlanTrigger {
    NodeJoined(NodeId),
    NodeDeparted(NodeId),
    ModelBecameAvailable { node_id: NodeId, model_id: String },
    ScheduledRebalance,
    LeaderElected(NodeId),
    FairnessThresholdExceeded(NodeId),
    ManualReplan,
}

// ─── Node Roles ──────────────────────────────────────────────

/// A node's assigned role within a MeshPlan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeRole {
    /// Runs an inference process for the throughput tier.
    ThroughputInference { model_id: String, port: u16 },
    /// Runs an inference process for the quality tier.
    QualityInference {
        model_id: String,
        port: u16,
        shard_index: Option<usize>,
    },
    /// Hosts corpus shards — no inference role on this plan.
    CorpusHost { shard_ids: Vec<String> },
    /// Runs the fast slot for this node's local requests.
    FastSlot { model_id: String },
    /// Contributes to the embed serving pool.
    EmbedServer { port: u16 },
    /// No active role in this plan.
    Standby,
}

// ─── Load Policy ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadPolicy {
    /// Round-robin across instance nodes.
    RoundRobin,
    /// Prefer local node, fall back to others.
    LocalFirst,
    /// Weighted by available memory headroom.
    MemoryWeighted,
}

// ─── Request Routing ─────────────────────────────────────────

/// Per-request routing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRouter {
    pub default_tier: Tier,
    pub routing_rules: Vec<RoutingRule>,
}

/// The tier a request is routed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    Quality,
    Throughput,
    FastSlot,
}

/// A routing rule that upgrades or downgrades a request's tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub condition: RoutingCondition,
    pub target: Tier,
    pub priority: u8,
}

/// Condition for a routing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoutingCondition {
    /// Token budget exceeds threshold — complex request.
    MaxTokensAbove(u32),
    /// Request explicitly tagged for quality tier.
    QualityHint,
    /// Quality tier queue depth is under threshold — no wait.
    QualityQueueDepthBelow(usize),
    /// Requester's fairness ledger is in credit.
    RequesterInCredit,
}

/// Tier routing configuration within a Tiered strategy.
pub type TierRouter = RequestRouter;

// ─── Queue Depths ────────────────────────────────────────────

/// Per-node queue depth report — used by the router to decide
/// whether to downgrade quality requests to throughput.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierQueueDepths {
    pub node_id: NodeId,
    pub quality_depth: usize,
    pub throughput_depth: usize,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_plan_serde_roundtrip() {
        let plan = MeshPlan {
            id: PlanId::generate(),
            computed_at: chrono::Utc::now(),
            trigger: PlanTrigger::ManualReplan,
            strategy: SchedulingStrategy::ParallelInstances {
                model_id: "qwen3_coder_next".into(),
                instance_nodes: vec![NodeId::from_u128(1), NodeId::from_u128(2)],
                load_policy: LoadPolicy::LocalFirst,
            },
            node_roles: HashMap::new(),
            router: RequestRouter {
                default_tier: Tier::Throughput,
                routing_rules: vec![],
            },
            version: 1,
        };

        let json = serde_json::to_string(&plan).unwrap();
        let back: MeshPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 1);
    }

    #[test]
    fn tiered_strategy_serde_roundtrip() {
        let strategy = SchedulingStrategy::Tiered {
            quality: Box::new(SchedulingStrategy::Unavailable {
                reason: UnavailableReason::NoModels,
            }),
            throughput: Box::new(SchedulingStrategy::ParallelInstances {
                model_id: "test".into(),
                instance_nodes: vec![],
                load_policy: LoadPolicy::RoundRobin,
            }),
            router: RequestRouter {
                default_tier: Tier::Throughput,
                routing_rules: vec![RoutingRule {
                    condition: RoutingCondition::MaxTokensAbove(4096),
                    target: Tier::Quality,
                    priority: 10,
                }],
            },
        };

        let json = serde_json::to_string(&strategy).unwrap();
        let back: SchedulingStrategy = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, SchedulingStrategy::Tiered { .. }));
    }

    #[test]
    fn plan_trigger_equality() {
        let a = PlanTrigger::ManualReplan;
        let b = PlanTrigger::ManualReplan;
        assert_eq!(a, b);

        let c = PlanTrigger::NodeJoined(NodeId::from_u128(1));
        let d = PlanTrigger::NodeJoined(NodeId::from_u128(1));
        assert_eq!(c, d);
    }
}
