// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mesh-membership tests.
//!
//! This file used to assemble the real host node — the daemon's `AppState`
//! with `client_router`/`internal_router` — and drive its HTTP routes. That is
//! the assembled host, not the OICP/contracts seam this crate simulates against
//! (`[[forbid]] sovereign-mesh-test-harness -> sovereign-daemon`), so those
//! tests are STOPPED. Each drove a route the harness cannot serve:
//!
//!   POST /v1/chat/completions     inference_e2e_with_mock_llama_server,
//!                                 inference_e2e_rejects_local_only_privacy,
//!                                 inference_503_retry_after_on_backend_failure,
//!                                 oicp_routing_selects_correct_model,
//!                                 omo_model_alias_routes_to_coding_model,
//!                                 unknown_model_name_falls_through_to_default
//!   GET  /status                  status_endpoint_reflects_mesh_state
//!   GET  /oicp/v1/capabilities    oicp_capabilities_returns_registered_models
//!   GET  /v1/models               models_endpoint_lists_registered_models
//!   POST /v1/knowledge/search     knowledge_search_returns_results_for_assigned_corpora,
//!                                 knowledge_search_empty_when_no_shards
//!   POST /internal/gossip         internal_gossip_endpoint_accepts_payload
//!   GET  /internal/latency/probe  internal_latency_probe_responds
//!   POST /internal/node/activity  node_activity_endpoint_returns_204_for_all_known_levels,
//!                                 node_activity_hot_then_idle_reflected_in_gossip_response
//!
//! The route coverage lives in `sovereign-daemon`'s own test tree:
//! `chat_completion_e2e.rs`/`openai_wire_fidelity.rs` (chat),
//! `models_http_e2e.rs`, `client_auth.rs` (capabilities/status),
//! `gossip_route.rs`, `knowledge_fanout_e2e.rs`, plus the daemon's own
//! `src/tests/server.rs` (`/internal/latency/probe`) and
//! `src/routes_internal/mesh_admin/tests.rs` (`/internal/node/activity`).
//! `MockLlamaServer` (`src/mock_llama.rs`) is left with no consumer here.

use commonwealth_core::mesh::NodeStatus;
use commonwealth_discovery::membership;

// ============================================================================
// Scenario: Mesh Formation (Phase 2)
// Init mesh, nodes join, verify member state converged.
// ============================================================================

#[test]
fn mesh_formation_two_nodes() {
    let (mut mesh, join_key) = membership::init_mesh(
        "Test Co-op",
        "Alice's Desktop",
        vec!["127.0.0.1:9742".parse().unwrap()],
    );

    assert_eq!(mesh.members.len(), 1);
    let alice_id = *mesh.members.keys().next().unwrap();

    // Bob joins.
    let bob_id = membership::accept_join(
        &mut mesh,
        &join_key,
        "Bob's Build",
        vec!["192.168.1.2:9742".parse().unwrap()],
        alice_id,
    )
    .unwrap();

    assert_eq!(mesh.members.len(), 2);
    assert_eq!(mesh.members[&bob_id].invited_by, alice_id);
    assert_eq!(mesh.members[&bob_id].status, NodeStatus::Online);
}

#[test]
fn mesh_formation_five_nodes() {
    let (mut mesh, join_key) = membership::init_mesh("Five Node Mesh", "Node 1", vec![]);
    let founder_id = *mesh.members.keys().next().unwrap();

    for i in 2..=5 {
        membership::accept_join(
            &mut mesh,
            &join_key,
            &format!("Node {i}"),
            vec![],
            founder_id,
        )
        .unwrap();
    }

    assert_eq!(mesh.members.len(), 5);
    // All should be online.
    assert!(mesh
        .members
        .values()
        .all(|m| m.status == NodeStatus::Online));
}

#[test]
fn mesh_formation_rejects_wrong_key() {
    let (mut mesh, _key) = membership::init_mesh("Test", "Alice", vec![]);
    let alice_id = *mesh.members.keys().next().unwrap();

    let result = membership::accept_join(&mut mesh, "cwth-0000-0000-0000", "Eve", vec![], alice_id);
    assert!(result.is_err());
    assert_eq!(mesh.members.len(), 1); // Eve was not added.
}
