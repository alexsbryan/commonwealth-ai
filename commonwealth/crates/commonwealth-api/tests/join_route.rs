//! Integration tests for the `/internal/join` handshake route.
//!
//! Exercises the route via tower's `oneshot` pattern rather than a
//! real TCP bind — same approach as existing tests in server.rs,
//! but isolated here so this test file doesn't share build state
//! with a pre-existing ModelInfo drift issue in those inline tests.
use std::collections::HashMap;
use std::net::SocketAddr;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_discovery::membership;
use tower::ServiceExt;

/// Build a mesh with a known founder and a known join_key. Returns
/// the AppState + the raw join_key so tests can mix them.
fn mesh_with_known_key() -> (AppState, String) {
    let join_key = membership::generate_join_key();
    let join_key_hash = membership::hash_join_key(&join_key);
    let founder_id = NodeId::generate();
    let founder_addr: SocketAddr = "192.168.1.10:9742".parse().unwrap();

    let capabilities = NodeCapabilities {
        hardware: HardwareProfile {
            gpus: vec![],
            system_ram_gb: 0,
            cpu_cores: 0,
            total_storage_gb: 0,
            free_storage_gb: 0,
            network_bandwidth_mbps: None,
        },
        available: AvailableResources::default(),
        active_processes: vec![],
        hosted_corpora: vec![],
        reported_at: 0,
        inference_availability: 1.0,
    };

    let founder = MemberRecord {
        node_id: founder_id,
        name: "Founder".into(),
        invited_by: founder_id,
        joined_at: 0,
        last_seen: 0,
        status: NodeStatus::Online,
        capabilities,
        addresses: vec![founder_addr],
    };

    let mut members = HashMap::new();
    members.insert(founder_id, founder);

    let mesh = Mesh {
        id: MeshId::generate(),
        name: "Test Mesh".into(),
        join_key_hash,
        members,
        peers: vec![],
    };

    (AppState::new(founder_id, mesh), join_key)
}

fn post(body: serde_json::Value) -> Request<Body> {
    Request::post("/internal/join")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn join_with_valid_key_admits_new_member() {
    let (state, join_key) = mesh_with_known_key();
    let app = internal_router(state.clone());

    let response = app
        .oneshot(post(serde_json::json!({
            "join_key": join_key,
            "joining_node_name": "Joiner",
            "joining_node_addresses": ["192.168.1.20:9742"],
        })))
        .await
        .unwrap();

    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert_eq!(status, StatusCode::OK, "body was: {body_str}");

    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("assigned_node_id").is_some());
    assert!(json.get("mesh").is_some());

    // Founder's mesh state should now reflect the new member.
    let mesh = state.inner.mesh.read().await;
    assert_eq!(
        mesh.members.len(),
        2,
        "founder should have added the joiner"
    );
    assert!(
        mesh.members.values().any(|m| m.name == "Joiner"),
        "joiner should be present by name"
    );
}

#[tokio::test]
async fn join_with_wrong_key_returns_401_and_does_not_mutate_mesh() {
    let (state, _join_key) = mesh_with_known_key();
    let app = internal_router(state.clone());

    let response = app
        .oneshot(post(serde_json::json!({
            "join_key": "cwth-dead-beef-cafe",
            "joining_node_name": "Attacker",
            "joining_node_addresses": ["10.0.0.99:9742"],
        })))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["reason"].as_str().is_some());

    // Mesh should still have exactly the founder.
    let mesh = state.inner.mesh.read().await;
    assert_eq!(mesh.members.len(), 1, "rejected join must not add member");
}

#[tokio::test]
async fn join_with_malformed_key_returns_401() {
    // The route delegates format validation to `membership::accept_join`,
    // which rejects anything that doesn't hash to the stored hash —
    // including things that aren't valid cwth-XXXX-XXXX-XXXX format at
    // all. Verify the rejection path handles that cleanly.
    let (state, _join_key) = mesh_with_known_key();
    let app = internal_router(state);

    let response = app
        .oneshot(post(serde_json::json!({
            "join_key": "not-a-valid-key",
            "joining_node_name": "Confused",
            "joining_node_addresses": [],
        })))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
