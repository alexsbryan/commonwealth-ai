// SPDX-License-Identifier: AGPL-3.0-or-later
//! After the switch a peer still reaches this node's inference, through the
//! daemon's listener and on to serve (pb-svrn-dials-serve).
//!
//! The daemon holds no weights on the dialing path: its local inference is the
//! terminal arm in loopback mode, dialing serve. The row's premise: peers dial
//! `/oicp/v1/capabilities` and `/v1/chat/completions` on the daemon, so both
//! must answer from serve, or inbound peer inference goes dark. And the turn's
//! admission id (phase-b-27) is honoured from a caller on this host only: a
//! peer's never reaches serve, the local caller's does.
//!
//! serve is a stub here that answers chat and records each body it is sent.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::routing::post;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use serde_json::{json, Value};
use sovereign_contracts::engine_state::ServedSelf;
use sovereign_contracts::oicp::ResidentSlot;
use sovereign_daemon::serve_client::{loopback_provider, ServeBase, ServeBaseSource};
use sovereign_daemon::server::client_router;
use sovereign_daemon::slot_manifest::CoreSlotManifest;
use sovereign_daemon::state::{AppState, LocalInferenceService, ServingSeed};
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

use crate::common::{id_to_hex, member_with_last_seen, spawn_router};

const SERVED_MODEL: &str = "served-primary";

/// serve's chat route: answers, and keeps every request body it was sent.
async fn stub_serve(seen: Arc<Mutex<Vec<Value>>>) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let seen = Arc::clone(&seen);
            async move {
                seen.lock().unwrap().push(body);
                axum::Json(json!({
                    "id": "chatcmpl-serve",
                    "object": "chat.completion",
                    "created": 0,
                    "model": SERVED_MODEL,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "answered by serve"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 3, "total_tokens": 4}
                }))
            }
        }),
    );
    format!("http://{}", spawn_router(app).await)
}

fn served() -> ServedSelf {
    ServedSelf {
        primary_model: SERVED_MODEL.into(),
        medium_model: SERVED_MODEL.into(),
        fast_model: SERVED_MODEL.into(),
        resident_slots: vec![ResidentSlot {
            role: "primary".into(),
            model_id: SERVED_MODEL.into(),
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        }],
        context_size: Some(8192),
        ..ServedSelf::default()
    }
}

/// Node A on the dialing path, with one peer in its roster.
fn node_a(serve_base: String) -> (AppState, NodeId) {
    let self_id = NodeId::from_u128(0x1111_1111_1111_1111 << 64);
    let peer_id = NodeId::from_u128(0x2222_2222_2222_2222 << 64);
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member_with_last_seen(self_id, "a", 100, "127.0.0.1:9742".parse().unwrap()),
    );
    members.insert(
        peer_id,
        member_with_last_seen(peer_id, "b", 100, "127.0.0.1:9876".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(42),
        name: "peer-to-serve".into(),
        invite_key_hash: [7u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    let serve = ServeBase {
        base: serve_base,
        source: ServeBaseSource::Default,
    };
    let arm = Arc::new(loopback_provider(&serve, served(), 4096));
    let adapter: Arc<dyn LocalInferenceService> = Arc::new(SovereignInferenceAdapter::new(
        arm,
        Arc::new(CoreSlotManifest),
    ));
    let state = AppState::new_with_serving(
        self_id,
        mesh,
        ServingSeed {
            local_inference: Some(adapter),
            ..Default::default()
        },
    );
    (state, peer_id)
}

async fn chat(client: &reqwest::Client, a: &SocketAddr, peer: Option<&NodeId>) -> Value {
    let mut req = client
        .post(format!("http://{a}/v1/chat/completions"))
        .json(&json!({
            "model": SERVED_MODEL,
            "messages": [{"role": "user", "content": "ping"}],
            "stream": false,
            "turn_admission": "turn-claimed",
        }));
    if let Some(peer) = peer {
        req = req.header("X-Node-Id", id_to_hex(peer));
    }
    let resp = req.send().await.expect("A answers");
    assert!(resp.status().is_success(), "A refused: {}", resp.status());
    resp.json().await.expect("a chat completion")
}

#[tokio::test]
async fn a_peer_turn_is_served_by_serve_through_the_daemon() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (state, peer_id) = node_a(stub_serve(Arc::clone(&seen)).await);
    let a = spawn_router(client_router(state)).await;
    let client = reqwest::Client::new();

    // What a peer reads before routing to A: serve's model, advertised by A.
    let caps: Value = client
        .get(format!("http://{a}/oicp/v1/capabilities"))
        .send()
        .await
        .expect("capabilities")
        .json()
        .await
        .expect("a manifest");
    let ids: Vec<&str> = caps["models"]
        .as_array()
        .map(|m| m.iter().filter_map(|m| m["id"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        ids.contains(&SERVED_MODEL),
        "A must advertise the model serve holds, or peers never route here: {ids:?}"
    );

    // The peer's turn, answered by serve.
    let answer = chat(&client, &a, Some(&peer_id)).await;
    assert_eq!(
        answer["choices"][0]["message"]["content"].as_str(),
        Some("answered by serve")
    );
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "the peer's turn reached serve once");
    assert!(
        bodies[0].get("turn_admission").is_none(),
        "a peer's admission claim must not reach serve: {}",
        bodies[0]
    );

    // The same request from this host keeps its admission id.
    chat(&client, &a, None).await;
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(
        bodies[1]["turn_admission"].as_str(),
        Some("turn-claimed"),
        "a caller on this host keeps its admission across the dial to serve"
    );
}
