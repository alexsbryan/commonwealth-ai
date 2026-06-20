// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for the `/internal/gossip` push-pull handler.
//!
//! Verifies that two `AppState` instances on the same mesh (same
//! `mesh_id` + `join_key_hash`) can POST their `Mesh` at each other
//! and end up with a unioned member view — the mechanic that
//! converges persisted-but-diverged peers.
use std::collections::HashMap;
use std::net::SocketAddr;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use tower::ServiceExt;

fn member(id: NodeId, name: &str, last_seen: u64) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen,
        status: NodeStatus::Online,
        capabilities: NodeCapabilities {
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
            reported_at: last_seen,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],

            embed_model: None,
            benchmark: None,
            current_in_flight: None,
        },
        addresses: vec!["192.168.1.1:9742".parse::<SocketAddr>().unwrap()],
    }
}

fn mesh_with(mesh_id: MeshId, hash: [u8; 32], members: Vec<MemberRecord>) -> Mesh {
    let mut map = HashMap::new();
    for m in members {
        map.insert(m.node_id, m);
    }
    Mesh {
        id: mesh_id,
        name: "Test".into(),
        join_key_hash: hash,
        members: map,
        peers: vec![],
    }
}

/// Wire-format mirror of `commonwealth_api::routes_internal::MeshWire`
/// so tests can build request bodies without the type being public.
/// Keeps flat-Vec members so serde_json doesn't choke on NodeId keys.
#[derive(serde::Serialize)]
struct MeshWireBody<'a> {
    id: MeshId,
    name: &'a str,
    join_key_hash: [u8; 32],
    members: Vec<MemberRecord>,
    peers: Vec<commonwealth_core::mesh::MeshPeering>,
}

fn gossip_request_body(mesh: &Mesh) -> serde_json::Value {
    let wire = MeshWireBody {
        id: mesh.id,
        name: &mesh.name,
        join_key_hash: mesh.join_key_hash,
        members: mesh.members.values().cloned().collect(),
        peers: mesh.peers.clone(),
    };
    serde_json::json!({ "mesh": wire })
}

#[tokio::test]
async fn gossip_merges_incoming_member_into_local_view() {
    let mesh_id = MeshId::from_u128(7);
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1); // self
    let node_b = NodeId::from_u128(2); // will be learned via gossip

    // Local mesh: only `A` knows about itself.
    let local = mesh_with(mesh_id, hash, vec![member(node_a, "A", 100)]);
    let state = AppState::new(node_a, local);

    // Incoming: a peer's view that includes B.
    let incoming = mesh_with(
        mesh_id,
        hash,
        vec![member(node_a, "A", 100), member(node_b, "B", 200)],
    );

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&incoming)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // The response echoes OUR updated view, now containing both A and B.
    let returned_members = resp["mesh"]["members"].as_array().unwrap();
    assert_eq!(returned_members.len(), 2);

    // And the AppState itself was mutated.
    let mesh = state.inner.mesh.read().await;
    assert!(mesh.members.contains_key(&node_a));
    assert!(mesh.members.contains_key(&node_b));
}

#[tokio::test]
async fn gossip_rejects_wrong_mesh_id() {
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1);
    let local = mesh_with(MeshId::from_u128(1), hash, vec![member(node_a, "A", 10)]);
    let state = AppState::new(node_a, local);

    let foreign = mesh_with(
        MeshId::from_u128(999), // different mesh!
        hash,
        vec![member(NodeId::from_u128(99), "Intruder", 9999)],
    );

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&foreign)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let mesh = state.inner.mesh.read().await;
    assert_eq!(mesh.members.len(), 1, "reject must not mutate");
}

#[tokio::test]
async fn gossip_rejects_mismatched_join_key_hash() {
    let mesh_id = MeshId::from_u128(1);
    let node_a = NodeId::from_u128(1);
    let local = mesh_with(mesh_id, [3u8; 32], vec![member(node_a, "A", 10)]);
    let state = AppState::new(node_a, local);

    let fake = mesh_with(
        mesh_id,
        [9u8; 32], // attacker knows mesh_id but not join_key_hash
        vec![member(NodeId::from_u128(99), "Intruder", 9999)],
    );

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&fake)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn gossip_does_not_overwrite_self_record() {
    // A buggy or malicious peer ships us a stale view of ourselves
    // (wrong name, Offline status). We're authoritative for self.
    let mesh_id = MeshId::from_u128(1);
    let hash = [3u8; 32];
    let me = NodeId::from_u128(1);

    let local = mesh_with(mesh_id, hash, vec![member(me, "Real-Me", 100)]);
    let state = AppState::new(me, local);

    let bogus_view_of_self = {
        let mut m = member(me, "Wrong-Name", 999999);
        m.status = NodeStatus::Offline;
        m
    };
    let incoming = mesh_with(mesh_id, hash, vec![bogus_view_of_self]);

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&incoming)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mesh = state.inner.mesh.read().await;
    let my_record = mesh.members.get(&me).unwrap();
    assert_eq!(my_record.name, "Real-Me");
    assert_eq!(my_record.status, NodeStatus::Online);
    assert_eq!(my_record.last_seen, 100);
}
