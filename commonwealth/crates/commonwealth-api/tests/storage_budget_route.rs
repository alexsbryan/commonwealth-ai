// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for `/internal/storage/budget`.
//!
//! Confirms the round-trip the desktop relies on: GET reports a
//! sensible default snapshot, POST sets / clears the budget, the
//! `AppState` atomic is updated, and the budget enforcement helper
//! (`storage_remaining_bytes`) saturates correctly when usage has
//! already exceeded the user's chosen ceiling.
//!
//! These tests live as integration tests rather than unit tests in
//! `mesh_admin.rs` because the `commonwealth-api` lib test target is
//! currently blocked by unrelated middleware build errors. The
//! integration path builds the lib (which is healthy) and runs
//! against the public surface.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use std::collections::HashMap;
use tower::ServiceExt;

fn fresh_state() -> AppState {
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "Test Mesh".into(),
        join_key_hash: [0u8; 32],
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    };
    AppState::new(NodeId::from_u128(1), mesh)
}

async fn get_storage_budget(state: AppState) -> (StatusCode, serde_json::Value) {
    let app = internal_router(state);
    let req = Request::builder()
        .method("GET")
        .uri("/internal/storage/budget")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn post_storage_budget(
    state: AppState,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let app = internal_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/internal/storage/budget")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn get_reports_unset_budget_with_recommendation() {
    // Default state: no budget. The route still reports a recommended
    // baseline so the desktop's "Use recommended" affordance has a
    // concrete value to apply on first launch.
    let (status, body) = get_storage_budget(fresh_state()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["budget_bytes"].is_null());
    assert_eq!(body["used_bytes"].as_u64().unwrap(), 0);
    // 1 GiB minimum even on tight disks (the AppState floor).
    assert!(body["recommended_bytes"].as_u64().unwrap() >= 1_073_741_824);
    // Free disk on a developer machine is always > 0.
    assert!(body["free_disk_bytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn post_round_trips_through_state() {
    let state = fresh_state();
    let budget = 50_u64 * 1_073_741_824; // 50 GiB

    let (status, body) =
        post_storage_budget(state.clone(), serde_json::json!({ "budget_bytes": budget })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["budget_bytes"].as_u64().unwrap(), budget);
    assert_eq!(state.storage_budget_bytes(), Some(budget));

    // GET after POST should reflect the same value.
    let (_, get_body) = get_storage_budget(state).await;
    assert_eq!(get_body["budget_bytes"].as_u64().unwrap(), budget);
}

#[tokio::test]
async fn post_null_clears_budget() {
    let state = fresh_state();
    state
        .set_storage_budget_bytes(Some(50_u64 * 1_073_741_824))
        .unwrap();
    assert!(state.storage_budget_bytes().is_some());

    let (status, body) =
        post_storage_budget(state.clone(), serde_json::json!({ "budget_bytes": null })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["budget_bytes"].is_null());
    assert_eq!(state.storage_budget_bytes(), None);
}

#[tokio::test]
async fn post_rejects_below_one_gib() {
    let (status, body) =
        post_storage_budget(fresh_state(), serde_json::json!({ "budget_bytes": 1 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("≥ 1 GiB"));
}

#[tokio::test]
async fn budget_remaining_saturates_at_zero_when_used_exceeds() {
    // The capabilities clamp enforces the budget by reading
    // `storage_remaining_bytes`. If a user lowers the budget below
    // current usage, this MUST return Some(0) — not panic, not
    // underflow — so the published `free_storage_gb` becomes 0 and
    // every scheduler stops assigning new work to this node. This
    // is the load-bearing invariant for the whole feature.
    let state = fresh_state();
    state
        .set_storage_budget_bytes(Some(50_u64 * 1_073_741_824))
        .unwrap();
    state.set_storage_used_bytes(80_u64 * 1_073_741_824);
    assert_eq!(state.storage_remaining_bytes(), Some(0));
}

#[tokio::test]
async fn budget_unset_returns_no_remaining() {
    // No budget configured ⇒ no clamp. The capabilities builder
    // must distinguish "no budget" (don't clamp anything) from
    // "budget of zero" (which the AppState setter rejects anyway).
    let state = fresh_state();
    assert_eq!(state.storage_budget_bytes(), None);
    assert_eq!(state.storage_remaining_bytes(), None);
}

// ── End-to-end: budget flows into scheduler decisions ─────────
//
// The contract this feature relies on is: when a node's published
// `free_storage_gb` is `0`, the LIVE collaborative-ingestion planner
// (`plan_collaborative_ingestion` — the scheduler corpus_collaborate
// actually drives) refuses to assign work to it. That contract is
// what makes "clamp `free_storage_gb` to budget remaining" sufficient
// to enforce the budget across the whole mesh, with no scheduler
// changes at all. (Re-pinned 2026-06-10 against the live planner when
// the dead `assign_knowledge_shards` scheduler was deleted.)
//
// The capabilities clamp is unit-tested in
// `sovereign-mesh::capabilities::tests`; here we close the loop by
// feeding the post-clamp value into the actual planner and asserting
// the user-visible outcome — no partition lands on the over-budget
// node.

use commonwealth_core::mesh::MemberRecord;
use commonwealth_core::oicp::EmbedModelInfo;
use commonwealth_inference::scheduler::knowledge_assignment::plan_collaborative_ingestion;
use corpus_engine::{SourceFileRecord, SourceFileStatus};

fn embed() -> EmbedModelInfo {
    use commonwealth_core::oicp::{NormalizationStrategy, PoolingStrategy};
    EmbedModelInfo {
        model_id: "qwen3-embedding-0.6b".into(),
        dimensions: 1024,
        pooling: PoolingStrategy::Mean,
        normalization: NormalizationStrategy::Application,
        query_instruction_prefix: String::new(),
    }
}

fn planner_member(id: u128, free_storage_gb: u32) -> MemberRecord {
    use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
    use commonwealth_core::mesh::NodeStatus;
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: NodeId::from_u128(id),
        name: format!("node-{id}"),
        invited_by: NodeId::from_u128(1),
        joined_at: 100,
        last_seen: 100,
        status: NodeStatus::Online,
        capabilities: NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 16,
                cpu_cores: 8,
                total_storage_gb: 500,
                free_storage_gb,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 100,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
            embed_model: Some(embed()),
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        },
        addresses: vec!["192.168.1.10:9742".parse().unwrap()],
    }
}

fn pending_files(n: usize) -> Vec<SourceFileRecord> {
    (0..n)
        .map(|i| SourceFileRecord {
            file_index: i,
            filename: format!("part-{i}.jsonl"),
            size_bytes: 1_000_000,
            status: SourceFileStatus::Pending,
        })
        .collect()
}

#[test]
fn planner_skips_node_whose_free_storage_was_clamped_to_zero() {
    // Alice (local) has plenty of disk; Bob has been clamped to 0 by
    // the capabilities layer because his operator's budget is already
    // saturated. The planner must keep every file off Bob.
    let alice = planner_member(1, 200);
    let bob = planner_member(2, 0);
    let handoff = plan_collaborative_ingestion(
        "wiki",
        "wikipedia-en",
        &pending_files(10),
        &alice,
        std::slice::from_ref(&bob),
        &embed(),
    )
    .expect("plan with one healthy node must succeed");
    assert!(
        handoff.partitions.iter().all(|p| p.node_id != bob.node_id),
        "planner assigned work to a budget-clamped node: {:?}",
        handoff.partitions
    );
    assert!(
        handoff
            .partitions
            .iter()
            .any(|p| p.node_id == alice.node_id),
        "planner should have placed the corpus on the node with headroom"
    );
}

#[test]
fn planner_skips_only_clamped_nodes_when_others_have_room() {
    // Alice (local) and Carol both have headroom, Bob is clamped.
    // The planner must use the healthy pair and never Bob.
    let alice = planner_member(1, 60);
    let bob = planner_member(2, 0); // budget-clamped
    let carol = planner_member(3, 80);
    let handoff = plan_collaborative_ingestion(
        "openalex",
        "openalex-works",
        &pending_files(40),
        &alice,
        &[bob.clone(), carol.clone()],
        &embed(),
    )
    .expect("plan with two healthy nodes must succeed");
    for p in &handoff.partitions {
        assert_ne!(
            p.node_id, bob.node_id,
            "budget-clamped node received a partition: {p:?}"
        );
    }
    assert!(
        handoff
            .partitions
            .iter()
            .any(|p| p.node_id == carol.node_id),
        "healthy peer should participate: {:?}",
        handoff.partitions
    );
}
