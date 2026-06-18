// SPDX-License-Identifier: AGPL-3.0-or-later
//! Receiver-side privacy guard for `/internal/app/state`.
//!
//! The gossip SENDER (`sovereign-mesh::gossip` Step 4) filters
//! local-only namespaces via `all_entries_for_gossip`. But this route
//! is reachable by any mesh peer over the internal mTLS port — and
//! mTLS proves the caller is *in the mesh*, not that it runs honest
//! code. A peer that is buggy or hostile can POST a local-only
//! `app_id`; without a receiver-side guard the entry lands in this
//! node's store, defeating the "private never crosses the wire"
//! guarantee from the RECEIVING end.
//!
//! These tests pin the symmetric guard added 2026-06-11: the receiver
//! refuses to merge any `GOSSIP_EXCLUDED_APP_IDS` namespace, while
//! still merging ordinary entries.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use tower::ServiceExt;

fn member(id: NodeId, name: &str, last_seen: u64) -> MemberRecord {
    MemberRecord {
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

fn single_node_mesh(node_a: NodeId) -> Mesh {
    let mut map = HashMap::new();
    map.insert(node_a, member(node_a, "A", 100));
    Mesh {
        id: MeshId::from_u128(7),
        name: "Test".into(),
        join_key_hash: [3u8; 32],
        members: map,
        peers: vec![],
    }
}

/// Build the `/internal/app/state` body a (possibly hostile) peer
/// would POST. `value_b64` is treated as raw UTF-8 by the receiver's
/// current stub decoder, matching production.
fn app_state_body(entries: &[(&str, &str, &str)]) -> serde_json::Value {
    let wire: Vec<serde_json::Value> = entries
        .iter()
        .map(|(app_id, key, value)| {
            serde_json::json!({
                "app_id": app_id,
                "key": key,
                "value_b64": value,
                "timestamp": 1000,
                // 16-byte NodeId as 32 hex chars.
                "origin_hex": "00000000000000000000000000000002",
            })
        })
        .collect();
    serde_json::json!({ "entries": wire })
}

async fn post_app_state(state: &AppState, body: serde_json::Value) -> StatusCode {
    let app = internal_router(state.clone());
    let resp = app
        .oneshot(
            Request::post("/internal/app/state")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

/// A peer POSTing local-only namespaces must NOT get them merged —
/// the privacy guarantee holds on the receiving end too.
#[tokio::test]
async fn receiver_rejects_gossiped_private_namespaces() {
    let node_a = NodeId::from_u128(1);
    let state = AppState::new(node_a, single_node_mesh(node_a));

    let status = post_app_state(
        &state,
        app_state_body(&[
            ("peer_preferences", "victim", "0.1"),
            ("activity-private", "usage", "tokens=999"),
            ("work-atlas-private", "session", "scope"),
            ("notes-private", "n1", "secret"),
            // One legitimate public entry rides along.
            ("contributions", "ev1", "served"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Every private namespace must be absent from the local store.
    for (app, key) in [
        ("peer_preferences", "victim"),
        ("activity-private", "usage"),
        ("work-atlas-private", "session"),
        ("notes-private", "n1"),
    ] {
        assert!(
            state.inner.mesh_store.get(app, key).unwrap().is_none(),
            "receiver merged a private namespace from the wire: {app}/{key}"
        );
    }

    // The public entry was accepted — the guard rejects only the
    // local-only namespaces, not all traffic.
    assert!(
        state
            .inner
            .mesh_store
            .get("contributions", "ev1")
            .unwrap()
            .is_some(),
        "receiver dropped a legitimate public entry"
    );
}

/// Ordinary (non-excluded) entries still merge normally — the guard
/// must not be a blanket reject.
#[tokio::test]
async fn receiver_accepts_ordinary_namespaces() {
    let node_a = NodeId::from_u128(1);
    let state = AppState::new(node_a, single_node_mesh(node_a));

    let status = post_app_state(
        &state,
        app_state_body(&[("inference", "plan1", "{}"), ("knowledge", "k1", "{}")]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert!(state.inner.mesh_store.get("inference", "plan1").unwrap().is_some());
    assert!(state.inner.mesh_store.get("knowledge", "k1").unwrap().is_some());
}
