//! Adaptive mesh scheduler integration tests.
//!
//! These tests exercise the full scheduler pipeline against the
//! `SimulatedMesh` harness: strategy selection, role assignment,
//! plan propagation, and topology transitions.

use std::collections::HashMap;

use commonwealth_core::capabilities::ComputeType;
use commonwealth_core::ids::NodeId;
use commonwealth_inference::plan::*;
use commonwealth_inference::scheduler::adaptive::{NodeProfile, SchedulerConfig};
use commonwealth_test_harness::simulated_mesh::SimulatedMesh;
use commonwealth_test_harness::simulated_node::SimulatedNodeBuilder;

/// Helper: register model names on nodes so the scheduler can detect
/// which models each node has available. The scheduler's `NodeProfile`
/// uses model name strings, not ModelIds.
fn register_models_on_profiles(profiles: &mut HashMap<NodeId, NodeProfile>, models: &[&str]) {
    for profile in profiles.values_mut() {
        profile.model_ids = models.iter().map(|s| s.to_string()).collect();
    }
}

fn register_models_on_node(
    profiles: &mut HashMap<NodeId, NodeProfile>,
    node_id: NodeId,
    models: &[&str],
) {
    if let Some(p) = profiles.get_mut(&node_id) {
        p.model_ids = models.iter().map(|s| s.to_string()).collect();
    }
}

// ═══════════════════════════════════════════════════════════════
// T-I01 — Parallel to Tiered on high-memory join
// ═══════════════════════════════════════════════════════════════

#[test]
fn ti01_parallel_to_tiered_on_high_memory_join() {
    // Start with 10 homogeneous 36GB nodes.
    let mut mesh = SimulatedMesh::new("ti01");
    for i in 0..10 {
        let node = SimulatedNodeBuilder::new(i + 1, &format!("node-{i}"))
            .gpu("M3 Pro", 36, ComputeType::Metal)
            .ram_gb(36);
        mesh.add_node(node);
    }

    let scheduler = mesh.make_scheduler();
    let mut profiles = mesh.scheduler_profiles();

    // All nodes have the throughput model.
    register_models_on_profiles(&mut profiles, &["qwen3_coder_next"]);

    // Initial plan: should be ParallelInstances.
    let initial_plan = scheduler
        .replan(PlanTrigger::LeaderElected(scheduler.node_id), &profiles)
        .unwrap();

    assert!(
        matches!(
            initial_plan.strategy,
            SchedulingStrategy::ParallelInstances { .. }
        ),
        "Initial plan should be ParallelInstances: {:?}",
        initial_plan.strategy
    );

    // Propagate to all nodes.
    mesh.propagate_mesh_plan(&initial_plan);

    // Priya joins with 128GB and GLM-5.1.
    let priya_id = NodeId::from_u128(999);
    profiles.insert(
        priya_id,
        NodeProfile {
            available_memory_gb: 128,
            model_ids: vec!["glm_5_1".into(), "qwen3_coder_next".into()],
        },
    );

    // Replan with the new node.
    let updated_plan = scheduler
        .replan(PlanTrigger::NodeJoined(priya_id), &profiles)
        .unwrap();

    assert!(
        matches!(updated_plan.strategy, SchedulingStrategy::Tiered { .. }),
        "After high-memory join: should be Tiered: {:?}",
        updated_plan.strategy
    );

    // Priya's node must be in the quality tier.
    let priya_roles = updated_plan.node_roles.get(&priya_id).unwrap();
    assert!(
        priya_roles
            .iter()
            .any(|r| matches!(r, NodeRole::QualityInference { .. })),
        "Priya's node must have QualityInference role"
    );

    // Plan version must have incremented.
    assert!(
        updated_plan.version > initial_plan.version,
        "Plan version must increase"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-I02 — Quality tier dissolves on departure
// ═══════════════════════════════════════════════════════════════

#[test]
fn ti02_quality_tier_dissolves_on_departure() {
    let mut mesh = SimulatedMesh::new("ti02");

    // 5 nodes + 1 high-memory node.
    for i in 0..5 {
        let node = SimulatedNodeBuilder::new(i + 1, &format!("node-{i}"))
            .gpu("M3 Pro", 36, ComputeType::Metal)
            .ram_gb(36);
        mesh.add_node(node);
    }

    let priya_id = NodeId::from_u128(999);
    let node = SimulatedNodeBuilder::new(999, "priya-studio")
        .gpu("M3 Max", 128, ComputeType::Metal)
        .ram_gb(128);
    mesh.add_node(node);

    let scheduler = mesh.make_scheduler();
    let mut profiles = mesh.scheduler_profiles();

    register_models_on_profiles(&mut profiles, &["qwen3_coder_next"]);
    register_models_on_node(&mut profiles, priya_id, &["glm_5_1", "qwen3_coder_next"]);

    // Plan should be Tiered.
    let tiered_plan = scheduler
        .replan(PlanTrigger::LeaderElected(scheduler.node_id), &profiles)
        .unwrap();

    assert!(
        matches!(tiered_plan.strategy, SchedulingStrategy::Tiered { .. }),
        "Should be Tiered with high-memory node"
    );

    // Priya departs.
    profiles.remove(&priya_id);

    let fallback_plan = scheduler
        .replan(PlanTrigger::NodeDeparted(priya_id), &profiles)
        .unwrap();

    assert!(
        matches!(
            fallback_plan.strategy,
            SchedulingStrategy::ParallelInstances { .. }
        ),
        "Should revert to ParallelInstances after departure: {:?}",
        fallback_plan.strategy
    );

    // Priya must not appear in the new plan.
    assert!(
        !fallback_plan.node_roles.contains_key(&priya_id),
        "Departed node must not appear in new plan"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-I03 — Abrupt departure triggers replan
// ═══════════════════════════════════════════════════════════════

#[test]
fn ti03_abrupt_departure_triggers_replan() {
    let mut mesh = SimulatedMesh::new("ti03");

    for i in 0..5 {
        let node = SimulatedNodeBuilder::new(i + 1, &format!("node-{i}"))
            .gpu("M3 Pro", 36, ComputeType::Metal)
            .ram_gb(36);
        mesh.add_node(node);
    }

    let priya_id = NodeId::from_u128(999);
    let node = SimulatedNodeBuilder::new(999, "priya")
        .gpu("M3 Max", 128, ComputeType::Metal)
        .ram_gb(128);
    mesh.add_node(node);

    let scheduler = mesh.make_scheduler();
    let mut profiles = mesh.scheduler_profiles();

    register_models_on_profiles(&mut profiles, &["qwen3_coder_next"]);
    register_models_on_node(&mut profiles, priya_id, &["glm_5_1", "qwen3_coder_next"]);

    // Establish tiered plan.
    let tiered = scheduler
        .replan(PlanTrigger::LeaderElected(scheduler.node_id), &profiles)
        .unwrap();
    assert!(matches!(tiered.strategy, SchedulingStrategy::Tiered { .. }));

    // Abrupt departure — remove from profiles (no graceful shutdown).
    profiles.remove(&priya_id);

    // Replan as if fault detector detected the departure.
    let after_departure = scheduler
        .replan(PlanTrigger::NodeDeparted(priya_id), &profiles)
        .unwrap();

    assert!(
        matches!(
            after_departure.strategy,
            SchedulingStrategy::ParallelInstances { .. }
        ),
        "Should revert after abrupt departure"
    );

    assert!(
        !after_departure.node_roles.contains_key(&priya_id),
        "Departed node must not appear in new plan"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-I04 — Simultaneous joins produce bounded replans
// ═══════════════════════════════════════════════════════════════

#[test]
fn ti04_simultaneous_joins_bounded_replans() {
    let mut mesh = SimulatedMesh::new("ti04");

    let node = SimulatedNodeBuilder::new(1, "node-0")
        .gpu("M3 Pro", 36, ComputeType::Metal)
        .ram_gb(36);
    mesh.add_node(node);

    let scheduler = mesh.make_scheduler();
    let mut profiles = mesh.scheduler_profiles();
    register_models_on_profiles(&mut profiles, &["qwen3_coder_next"]);

    // Initial plan.
    let plan1 = scheduler
        .replan(PlanTrigger::LeaderElected(scheduler.node_id), &profiles)
        .unwrap();

    // Three nodes join rapidly — each triggers a replan.
    for i in 0..3 {
        let id = NodeId::from_u128(100 + i);
        profiles.insert(
            id,
            NodeProfile {
                available_memory_gb: 36,
                model_ids: vec!["qwen3_coder_next".into()],
            },
        );
    }

    // Single replan after all three join (simulating debounce coalescing).
    let plan_final = scheduler
        .replan(PlanTrigger::NodeJoined(NodeId::from_u128(102)), &profiles)
        .unwrap();

    // Version delta should be small — at most 2 (initial + coalesced replan).
    let version_delta = plan_final.version - plan1.version;
    assert!(
        version_delta <= 2,
        "Coalesced joins should produce bounded version delta (got {version_delta})"
    );

    // All 4 nodes should have throughput roles.
    if let SchedulingStrategy::ParallelInstances { instance_nodes, .. } = &plan_final.strategy {
        assert_eq!(
            instance_nodes.len(),
            4,
            "All 4 nodes should be in throughput tier"
        );
    } else {
        panic!("Expected ParallelInstances");
    }
}

// ═══════════════════════════════════════════════════════════════
// T-I05 — Only leader replans
// ═══════════════════════════════════════════════════════════════

#[test]
fn ti05_only_leader_replans() {
    let mut mesh = SimulatedMesh::new("ti05");

    // Node A (lower ID = leader) and Node B (higher ID = follower).
    let node_a = SimulatedNodeBuilder::new(1, "leader")
        .gpu("M3 Pro", 36, ComputeType::Metal)
        .ram_gb(36);
    let node_b = SimulatedNodeBuilder::new(100, "follower")
        .gpu("M3 Pro", 36, ComputeType::Metal)
        .ram_gb(36);
    mesh.add_node(node_a);
    mesh.add_node(node_b);

    let profiles = mesh.scheduler_profiles();

    // Scheduler on leader (node A) should succeed.
    let leader_scheduler = mesh.make_scheduler();
    let result =
        leader_scheduler.replan(PlanTrigger::LeaderElected(NodeId::from_u128(1)), &profiles);
    assert!(result.is_ok(), "Leader should be able to replan");

    // Scheduler on follower (node B) should fail.
    let mut follower_scheduler =
        commonwealth_inference::scheduler::adaptive::InferenceScheduler::new(
            NodeId::from_u128(100),
            SchedulerConfig::default(),
        );
    follower_scheduler.online_nodes = mesh.node_ids();

    let result = follower_scheduler.replan(
        PlanTrigger::LeaderElected(NodeId::from_u128(100)),
        &profiles,
    );
    assert!(
        result.is_err(),
        "Non-leader node must not be able to replan"
    );
}

// ═══════════════════════════════════════════════════════════════
// T-I06 — Full demo scenario arc
// ═══════════════════════════════════════════════════════════════

#[test]
fn ti06_demo_scenario_full_arc() {
    // Morning: 6 early risers join.
    let mut mesh = SimulatedMesh::new("demo");
    for i in 0..6 {
        let node = SimulatedNodeBuilder::new(i + 1, &format!("morning-{i}"))
            .gpu("M3 Pro", 36, ComputeType::Metal)
            .ram_gb(36);
        mesh.add_node(node);
    }

    let scheduler = mesh.make_scheduler();
    let mut profiles = mesh.scheduler_profiles();
    register_models_on_profiles(&mut profiles, &["qwen3_coder_next", "qwen3_0_6b"]);

    let morning_plan = scheduler
        .replan(PlanTrigger::LeaderElected(scheduler.node_id), &profiles)
        .unwrap();

    assert!(
        matches!(
            morning_plan.strategy,
            SchedulingStrategy::ParallelInstances { .. }
        ),
        "Morning: should be ParallelInstances"
    );

    // More engineers join — 18 nodes total.
    for i in 6..18 {
        let id = NodeId::from_u128(i + 1);
        profiles.insert(
            id,
            NodeProfile {
                available_memory_gb: 36,
                model_ids: vec!["qwen3_coder_next".into(), "qwen3_0_6b".into()],
            },
        );
    }

    let midmorning_plan = scheduler
        .replan(PlanTrigger::NodeJoined(NodeId::from_u128(18)), &profiles)
        .unwrap();

    if let SchedulingStrategy::ParallelInstances { instance_nodes, .. } = &midmorning_plan.strategy
    {
        assert_eq!(
            instance_nodes.len(),
            18,
            "All 18 nodes should serve throughput"
        );
    } else {
        panic!("Midmorning should be ParallelInstances");
    }

    // Priya joins with her Mac Studio.
    let priya_id = NodeId::from_u128(999);
    profiles.insert(
        priya_id,
        NodeProfile {
            available_memory_gb: 128,
            model_ids: vec![
                "glm_5_1".into(),
                "qwen3_coder_next".into(),
                "qwen3_0_6b".into(),
            ],
        },
    );

    let priya_plan = scheduler
        .replan(PlanTrigger::NodeJoined(priya_id), &profiles)
        .unwrap();

    assert!(
        matches!(priya_plan.strategy, SchedulingStrategy::Tiered { .. }),
        "After Priya joins: should be Tiered"
    );

    // Verify Priya is in the quality tier.
    let priya_roles = priya_plan.node_roles.get(&priya_id).unwrap();
    assert!(
        priya_roles
            .iter()
            .any(|r| matches!(r, NodeRole::QualityInference { .. })),
        "Priya must be in quality tier"
    );

    // Verify complex request routes to quality.
    use commonwealth_inference::tier_router::{route_request, RequestContext};
    let routing = route_request(
        &priya_plan.router,
        &RequestContext {
            max_tokens: 8192,
            quality_hint: false,
            quality_queue_depth: 0,
            requester_in_credit: true,
        },
    );
    assert_eq!(
        routing,
        Tier::Quality,
        "Complex request should route to quality tier"
    );

    // Evening: Priya closes her laptop.
    profiles.remove(&priya_id);
    let evening_plan = scheduler
        .replan(PlanTrigger::NodeDeparted(priya_id), &profiles)
        .unwrap();

    assert!(
        matches!(
            evening_plan.strategy,
            SchedulingStrategy::ParallelInstances { .. }
        ),
        "Evening: should revert to ParallelInstances"
    );

    // Complex request now goes to throughput (no quality tier available).
    let routing = route_request(
        &evening_plan.router,
        &RequestContext {
            max_tokens: 8192,
            quality_hint: false,
            quality_queue_depth: 0,
            requester_in_credit: true,
        },
    );
    assert_eq!(
        routing,
        Tier::Throughput,
        "Without quality tier, complex request should fall back to throughput"
    );

    // Plan versions should be monotonically increasing through the arc.
    assert!(morning_plan.version < midmorning_plan.version);
    assert!(midmorning_plan.version < priya_plan.version);
    assert!(priya_plan.version < evening_plan.version);
}
