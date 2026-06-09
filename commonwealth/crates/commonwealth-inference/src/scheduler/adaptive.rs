// SPDX-License-Identifier: AGPL-3.0-or-later
//! Adaptive mesh scheduler.
//!
//! Automatically chooses the best inference topology (parallel instances,
//! single-instance sharding, or tiered) based on the current mesh
//! composition, and transitions between strategies as nodes join and depart.
//!
//! Runs on the leader node only. Non-leaders watch for leadership changes
//! and start the scheduler if elected.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

use commonwealth_core::ids::{NodeId, PlanId};

use crate::plan::*;
use crate::scheduler::leader::elect_leader;
use crate::topology::TopologyEvent;

// ─── Configuration ───────────────────────────────────────────

pub struct SchedulerConfig {
    /// Memory threshold for quality tier eligibility (GB).
    pub quality_tier_min_memory_gb: u32,
    /// Seconds to wait after leader election before planning.
    /// Allows capability profiles to propagate via gossip.
    pub convergence_wait_secs: u64,
    /// Rebalance if load distribution is this uneven (0.0–1.0).
    pub rebalance_threshold: f32,
    /// The model ID used for the throughput tier.
    pub throughput_model_id: String,
    /// Minimum memory (GB) required for the throughput model.
    pub throughput_min_memory_gb: u32,
    /// The model ID used for the quality tier.
    pub quality_model_id: String,
    /// The model ID for the fast slot (every node gets this).
    pub fast_slot_model_id: String,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            quality_tier_min_memory_gb: 80,
            convergence_wait_secs: 15,
            rebalance_threshold: 0.3,
            throughput_model_id: "qwen3_coder_next".to_string(),
            throughput_min_memory_gb: 32,
            quality_model_id: "glm_5_1".to_string(),
            fast_slot_model_id: "qwen3_0_6b".to_string(),
        }
    }
}

// ─── Scheduler ───────────────────────────────────────────────

pub struct InferenceScheduler {
    pub node_id: NodeId,
    config: SchedulerConfig,
    /// Monotonically increasing plan version.
    version: Arc<AtomicU64>,
    /// All online node IDs — maintained by the event loop.
    pub online_nodes: Vec<NodeId>,
}

impl InferenceScheduler {
    pub fn new(node_id: NodeId, config: SchedulerConfig) -> Self {
        Self {
            node_id,
            config,
            version: Arc::new(AtomicU64::new(0)),
            online_nodes: Vec::new(),
        }
    }

    /// Whether this node is the current leader (lowest NodeId).
    pub fn is_leader(&self) -> bool {
        elect_leader(&self.online_nodes) == Some(self.node_id)
    }

    /// Recompute the MeshPlan in response to a topology change.
    /// Only runs on the leader node.
    pub fn replan(
        &self,
        trigger: PlanTrigger,
        profiles: &HashMap<NodeId, NodeProfile>,
    ) -> Result<MeshPlan, SchedulerError> {
        if !self.is_leader() {
            return Err(SchedulerError::NotLeader);
        }

        let strategy = self.choose_strategy(profiles)?;
        let roles = self.assign_roles(&strategy, profiles)?;
        let router = self.build_router(&strategy);

        let version = self.version.fetch_add(1, Ordering::SeqCst) + 1;

        let plan = MeshPlan {
            id: PlanId::generate(),
            computed_at: chrono::Utc::now(),
            trigger,
            strategy,
            node_roles: roles,
            router,
            version,
        };

        tracing::info!(
            version = version,
            trigger = ?plan.trigger,
            node_count = profiles.len(),
            "Mesh plan recomputed"
        );

        Ok(plan)
    }

    /// Phase 1: Choose the SchedulingStrategy for the current mesh.
    pub fn choose_strategy(
        &self,
        profiles: &HashMap<NodeId, NodeProfile>,
    ) -> Result<SchedulingStrategy, SchedulerError> {
        let quality_eligible: Vec<NodeId> = profiles
            .iter()
            .filter(|(_, p)| {
                p.available_memory_gb >= self.config.quality_tier_min_memory_gb
                    && p.model_ids.contains(&self.config.quality_model_id)
            })
            .map(|(id, _)| *id)
            .collect();

        let throughput_eligible: Vec<NodeId> = profiles
            .iter()
            .filter(|(_, p)| {
                p.available_memory_gb >= self.config.throughput_min_memory_gb
                    && p.model_ids.contains(&self.config.throughput_model_id)
            })
            .map(|(id, _)| *id)
            .collect();

        match (quality_eligible.is_empty(), throughput_eligible.is_empty()) {
            // No capable nodes at all.
            (true, true) => {
                let available_gb = profiles
                    .values()
                    .map(|p| p.available_memory_gb)
                    .max()
                    .unwrap_or(0);

                Ok(SchedulingStrategy::Unavailable {
                    reason: UnavailableReason::InsufficientMemory {
                        min_required_gb: self.config.throughput_min_memory_gb,
                        available_gb,
                    },
                })
            }

            // Only throughput tier possible.
            (true, false) => Ok(SchedulingStrategy::ParallelInstances {
                model_id: self.config.throughput_model_id.clone(),
                instance_nodes: throughput_eligible,
                load_policy: LoadPolicy::LocalFirst,
            }),

            // Only quality tier possible (unusual — high-mem nodes only).
            (false, true) => {
                // Use the first quality-eligible node for single instance.
                // In a real deployment, would use plan_builder for sharding.
                Ok(SchedulingStrategy::ParallelInstances {
                    model_id: self.config.quality_model_id.clone(),
                    instance_nodes: quality_eligible,
                    load_policy: LoadPolicy::LocalFirst,
                })
            }

            // Both tiers possible — build tiered strategy.
            (false, false) => {
                let quality = Box::new(SchedulingStrategy::ParallelInstances {
                    model_id: self.config.quality_model_id.clone(),
                    instance_nodes: quality_eligible,
                    load_policy: LoadPolicy::LocalFirst,
                });

                let throughput = Box::new(SchedulingStrategy::ParallelInstances {
                    model_id: self.config.throughput_model_id.clone(),
                    instance_nodes: throughput_eligible,
                    load_policy: LoadPolicy::LocalFirst,
                });

                let router = self.default_tier_router();

                Ok(SchedulingStrategy::Tiered {
                    quality,
                    throughput,
                    router,
                })
            }
        }
    }

    /// Phase 2: Assign concrete roles to each node.
    pub fn assign_roles(
        &self,
        strategy: &SchedulingStrategy,
        profiles: &HashMap<NodeId, NodeProfile>,
    ) -> Result<HashMap<NodeId, Vec<NodeRole>>, SchedulerError> {
        let mut roles: HashMap<NodeId, Vec<NodeRole>> = HashMap::new();

        // Every node gets a fast slot.
        for node_id in profiles.keys() {
            roles.entry(*node_id).or_default().push(NodeRole::FastSlot {
                model_id: self.config.fast_slot_model_id.clone(),
            });
        }

        self.assign_strategy_roles(strategy, &mut roles)?;

        // Remaining nodes not in any inference role get Standby.
        for node_id in profiles.keys() {
            let node_roles = roles.entry(*node_id).or_default();
            let has_inference = node_roles.iter().any(|r| {
                matches!(
                    r,
                    NodeRole::ThroughputInference { .. } | NodeRole::QualityInference { .. }
                )
            });
            if !has_inference {
                node_roles.push(NodeRole::Standby);
            }
        }

        Ok(roles)
    }

    fn assign_strategy_roles(
        &self,
        strategy: &SchedulingStrategy,
        roles: &mut HashMap<NodeId, Vec<NodeRole>>,
    ) -> Result<(), SchedulerError> {
        match strategy {
            SchedulingStrategy::ParallelInstances {
                model_id,
                instance_nodes,
                ..
            } => {
                for (i, &node_id) in instance_nodes.iter().enumerate() {
                    roles
                        .entry(node_id)
                        .or_default()
                        .push(NodeRole::ThroughputInference {
                            model_id: model_id.clone(),
                            port: 8100 + i as u16,
                        });
                }
            }

            SchedulingStrategy::SingleInstance { shard_plan } => {
                for (i, assignment) in shard_plan.assignments.iter().enumerate() {
                    roles
                        .entry(assignment.node_id)
                        .or_default()
                        .push(NodeRole::QualityInference {
                            model_id: shard_plan.model.to_string(),
                            port: assignment.rpc_address.port(),
                            shard_index: Some(i),
                        });
                }
            }

            SchedulingStrategy::Tiered {
                quality,
                throughput,
                ..
            } => {
                // Track which nodes get quality roles so we can exclude
                // them from throughput.
                let quality_nodes: Vec<NodeId> = match quality.as_ref() {
                    SchedulingStrategy::ParallelInstances { instance_nodes, .. } => {
                        instance_nodes.clone()
                    }
                    _ => vec![],
                };

                // Assign quality roles.
                self.assign_quality_roles(quality, roles)?;

                // Assign throughput roles, excluding quality-tier nodes.
                match throughput.as_ref() {
                    SchedulingStrategy::ParallelInstances {
                        model_id,
                        instance_nodes,
                        ..
                    } => {
                        for (i, &node_id) in instance_nodes.iter().enumerate() {
                            if quality_nodes.contains(&node_id) {
                                continue; // Don't double-assign
                            }
                            roles
                                .entry(node_id)
                                .or_default()
                                .push(NodeRole::ThroughputInference {
                                    model_id: model_id.clone(),
                                    port: 8100 + i as u16,
                                });
                        }
                    }
                    _ => {
                        self.assign_strategy_roles(throughput, roles)?;
                    }
                }
            }

            SchedulingStrategy::Unavailable { .. } => {
                // No inference roles.
            }
        }

        Ok(())
    }

    fn assign_quality_roles(
        &self,
        strategy: &SchedulingStrategy,
        roles: &mut HashMap<NodeId, Vec<NodeRole>>,
    ) -> Result<(), SchedulerError> {
        match strategy {
            SchedulingStrategy::ParallelInstances {
                model_id,
                instance_nodes,
                ..
            } => {
                for (i, &node_id) in instance_nodes.iter().enumerate() {
                    roles
                        .entry(node_id)
                        .or_default()
                        .push(NodeRole::QualityInference {
                            model_id: model_id.clone(),
                            port: 8200 + i as u16,
                            shard_index: None,
                        });
                }
            }
            SchedulingStrategy::SingleInstance { shard_plan } => {
                for (i, assignment) in shard_plan.assignments.iter().enumerate() {
                    roles
                        .entry(assignment.node_id)
                        .or_default()
                        .push(NodeRole::QualityInference {
                            model_id: shard_plan.model.to_string(),
                            port: assignment.rpc_address.port(),
                            shard_index: Some(i),
                        });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn build_router(&self, strategy: &SchedulingStrategy) -> RequestRouter {
        match strategy {
            SchedulingStrategy::Tiered { .. } => self.default_tier_router(),
            _ => RequestRouter {
                default_tier: Tier::Throughput,
                routing_rules: vec![],
            },
        }
    }

    fn default_tier_router(&self) -> RequestRouter {
        RequestRouter {
            default_tier: Tier::Throughput,
            routing_rules: vec![
                // Upgrade complex requests to quality tier.
                RoutingRule {
                    condition: RoutingCondition::MaxTokensAbove(4096),
                    target: Tier::Quality,
                    priority: 10,
                },
                // Upgrade explicitly tagged requests.
                RoutingRule {
                    condition: RoutingCondition::QualityHint,
                    target: Tier::Quality,
                    priority: 20,
                },
                // Downgrade if quality queue is backed up.
                RoutingRule {
                    condition: RoutingCondition::QualityQueueDepthBelow(3),
                    target: Tier::Quality,
                    priority: 5,
                },
            ],
        }
    }

    /// Start the scheduler event loop. Only the leader runs this.
    pub async fn run(
        &mut self,
        mut events: impl Stream<Item = TopologyEvent> + Unpin,
        profiles: &mut HashMap<NodeId, NodeProfile>,
        shutdown: CancellationToken,
    ) {
        // Wait for the convergence window after leader election.
        tokio::time::sleep(std::time::Duration::from_secs(
            self.config.convergence_wait_secs,
        ))
        .await;

        // Initial plan on startup.
        self.online_nodes = profiles.keys().copied().collect();
        if let Err(e) = self.replan(PlanTrigger::LeaderElected(self.node_id), profiles) {
            tracing::error!(error = %e, "Initial plan failed");
        }

        // Debounce rapid topology changes.
        let mut pending_trigger: Option<PlanTrigger> = None;
        let mut debounce = tokio::time::interval(std::time::Duration::from_secs(3));

        loop {
            tokio::select! {
                Some(event) = events.next() => {
                    let trigger = match event {
                        TopologyEvent::NodeJoined(id) => {
                            self.online_nodes.push(id);
                            PlanTrigger::NodeJoined(id)
                        }
                        TopologyEvent::NodeDeparted(id) => {
                            self.online_nodes.retain(|&n| n != id);
                            profiles.remove(&id);
                            PlanTrigger::NodeDeparted(id)
                        }
                        TopologyEvent::ModelAvailable { node_id, model_id } => {
                            PlanTrigger::ModelBecameAvailable { node_id, model_id }
                        }
                        TopologyEvent::LeaderChanged(id) if id == self.node_id => {
                            PlanTrigger::LeaderElected(id)
                        }
                        _ => continue,
                    };

                    if self.is_leader() {
                        pending_trigger = Some(trigger);
                    }
                }

                _ = debounce.tick() => {
                    if let Some(trigger) = pending_trigger.take() {
                        if let Err(e) = self.replan(trigger, profiles) {
                            tracing::error!(error = %e, "Replan failed");
                        }
                    }
                }

                _ = shutdown.cancelled() => break,
            }
        }
    }
}

// ─── Supporting types ────────────────────────────────────────

/// Simplified node profile for strategy selection.
/// Extracted from `NodeCapabilities` by the caller.
#[derive(Debug, Clone)]
pub struct NodeProfile {
    pub available_memory_gb: u32,
    pub model_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("Not the leader node")]
    NotLeader,
    #[error("No eligible nodes for strategy")]
    NoEligibleNodes,
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_scheduler() -> InferenceScheduler {
        InferenceScheduler {
            node_id: NodeId::from_u128(1),
            config: SchedulerConfig::default(),
            version: Arc::new(AtomicU64::new(0)),
            online_nodes: vec![NodeId::from_u128(1)],
        }
    }

    fn profile(memory_gb: u32, models: &[&str]) -> NodeProfile {
        NodeProfile {
            available_memory_gb: memory_gb,
            model_ids: models.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// T-S01: Homogeneous small mesh → ParallelInstances.
    #[test]
    fn ts01_homogeneous_small_mesh_parallel_instances() {
        let scheduler = make_test_scheduler();

        let profiles: HashMap<NodeId, NodeProfile> = (0..5)
            .map(|i| (NodeId::from_u128(i), profile(36, &["qwen3_coder_next"])))
            .collect();

        let strategy = scheduler.choose_strategy(&profiles).unwrap();

        assert!(
            matches!(strategy, SchedulingStrategy::ParallelInstances { .. }),
            "Homogeneous 36GB nodes should produce ParallelInstances: {strategy:?}"
        );

        if let SchedulingStrategy::ParallelInstances { instance_nodes, .. } = strategy {
            assert_eq!(
                instance_nodes.len(),
                5,
                "All 5 nodes should be in the instance pool"
            );
        }
    }

    /// T-S02: High-memory node → Tiered.
    #[test]
    fn ts02_tiered_when_high_memory_node_present() {
        let scheduler = make_test_scheduler();

        let mut profiles: HashMap<NodeId, NodeProfile> = (0..10)
            .map(|i| (NodeId::from_u128(i), profile(36, &["qwen3_coder_next"])))
            .collect();

        // Add one high-memory node with GLM-5.1.
        profiles.insert(
            NodeId::from_u128(99),
            profile(128, &["glm_5_1", "qwen3_coder_next"]),
        );

        let strategy = scheduler.choose_strategy(&profiles).unwrap();

        assert!(
            matches!(strategy, SchedulingStrategy::Tiered { .. }),
            "Should choose Tiered when high-memory node is present: {strategy:?}"
        );

        if let SchedulingStrategy::Tiered {
            quality,
            throughput,
            ..
        } = strategy
        {
            assert!(
                matches!(*quality, SchedulingStrategy::ParallelInstances { .. }),
                "Quality tier should be ParallelInstances"
            );
            assert!(
                matches!(*throughput, SchedulingStrategy::ParallelInstances { .. }),
                "Throughput tier should be ParallelInstances"
            );
        }
    }

    /// T-S03: Insufficient memory → Unavailable.
    #[test]
    fn ts03_unavailable_when_insufficient_memory() {
        let scheduler = make_test_scheduler();

        let profiles: HashMap<NodeId, NodeProfile> = (0..3)
            .map(|i| (NodeId::from_u128(i), profile(16, &[])))
            .collect();

        let strategy = scheduler.choose_strategy(&profiles).unwrap();

        assert!(
            matches!(strategy, SchedulingStrategy::Unavailable { .. }),
            "Should be Unavailable when all nodes are below minimum: {strategy:?}"
        );
    }

    /// T-S04: Quality-tier node excluded from throughput roles.
    #[test]
    fn ts04_quality_node_not_double_assigned() {
        let mut scheduler = make_test_scheduler();

        let quality_node = NodeId::from_u128(99);

        let mut profiles: HashMap<NodeId, NodeProfile> = (0..5)
            .map(|i| (NodeId::from_u128(i), profile(36, &["qwen3_coder_next"])))
            .collect();

        profiles.insert(quality_node, profile(128, &["glm_5_1", "qwen3_coder_next"]));

        // Make scheduler the leader by ensuring it's in online_nodes.
        scheduler.online_nodes = profiles.keys().copied().collect();

        let strategy = scheduler.choose_strategy(&profiles).unwrap();
        let roles = scheduler.assign_roles(&strategy, &profiles).unwrap();

        let quality_node_roles = roles.get(&quality_node).unwrap();

        // Must have QualityInference.
        assert!(
            quality_node_roles
                .iter()
                .any(|r| matches!(r, NodeRole::QualityInference { .. })),
            "Quality node must have QualityInference role"
        );

        // Must NOT have ThroughputInference.
        assert!(
            !quality_node_roles
                .iter()
                .any(|r| matches!(r, NodeRole::ThroughputInference { .. })),
            "Quality node must not also run throughput inference"
        );

        // Must have FastSlot.
        assert!(
            quality_node_roles
                .iter()
                .any(|r| matches!(r, NodeRole::FastSlot { .. })),
            "Quality node must have FastSlot"
        );
    }

    /// T-S05: Plan version increments monotonically.
    #[test]
    fn ts05_plan_version_monotonic() {
        let mut scheduler = make_test_scheduler();
        scheduler.online_nodes = vec![NodeId::from_u128(1)];

        let profiles: HashMap<NodeId, NodeProfile> =
            [(NodeId::from_u128(1), profile(36, &["qwen3_coder_next"]))]
                .into_iter()
                .collect();

        let plan1 = scheduler
            .replan(PlanTrigger::ManualReplan, &profiles)
            .unwrap();
        let plan2 = scheduler
            .replan(PlanTrigger::ManualReplan, &profiles)
            .unwrap();
        let plan3 = scheduler
            .replan(PlanTrigger::ManualReplan, &profiles)
            .unwrap();

        assert!(
            plan2.version > plan1.version,
            "Version must increase: {} > {}",
            plan2.version,
            plan1.version
        );
        assert!(
            plan3.version > plan2.version,
            "Version must increase: {} > {}",
            plan3.version,
            plan2.version
        );
    }
}
