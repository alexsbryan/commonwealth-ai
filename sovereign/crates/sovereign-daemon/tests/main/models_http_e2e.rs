// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET /v1/models` HTTP-surface integration test.
//!
//! `daemon::tests::register_local_model_slots_writes_info_for_all_three_slots`
//! covers the daemon-side write (ModelInfo lands in `inference_store`).
//! What's not pinned: does the HTTP handler at
//! `routes_inference::list_models` actually read those entries and
//! return them in the OpenAI-shape envelope?
//!
//! Why this matters: `/v1/models` is the discovery surface every
//! OpenAI-compatible client (opencode, codex, anthropic-sdk pointing
//! at a local proxy, etc.) hits to populate its model picker. A
//! regression here makes the daemon look empty to every client, even
//! when models are loaded and would serve.
//!
//! The pre-existing daemon-side test catches the case "models aren't
//! written to the store"; this test catches "models are written but
//! the wire path drops them" — different layer, different failure
//! modes (origin filtering, alias synthesis, mesh-liveness join).
//!
//! **Scope narrowed 2026-08-27: this exercises the ORCHESTRATOR path.**
//! `build_state` installs no `local_inference`, so `list_models` falls
//! through to `store_rows` — the store scan these assertions were written
//! against. A node WITH local inference (every Sovereign mesh node) no
//! longer reads the store at all: it lists from the OICP manifests, which
//! is the same source `locate_named_model` resolves against. The store's
//! origin-liveness filter tested here answers "is the entry's last writer
//! reachable", which is not "can anything serve this" — that gap is why
//! `/v1/models` advertised ids chat completions refused. The manifest
//! path's own gate lives in
//! `sovereign_daemon::routes_inference::list_models_tests`.
//!
//! Two assertions:
//!
//! 1. **Locally-loaded model surfaces in `/v1/models`.** Insert a
//!    `ModelInfo` keyed by self's `NodeId` → 200 response → the
//!    model's name appears in `data[]`. Regression target: the
//!    origin-filter (`live_nodes.contains(origin)`) silently drops
//!    self's own loaded models.
//! 2. **Peer-only model from offline peer is filtered out.** Insert
//!    a `ModelInfo` keyed by a NodeId not in the mesh's online set,
//!    AND not locally loaded → `/v1/models` must omit it. This is
//!    the user-visible enforcement of the project memo
//!    `project_v1_models_liveness.md` ("the /v1/models liveness
//!    filter TODO") — pinning the half that's already implemented so
//!    a future regression that bypassed the filter shows up
//!    immediately.
use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_core::mesh::Mesh;
use kernel_types::{MeshId, ModelId, NodeId};
use oicp_types::model_catalog::{ModelArchitecture, ModelInfo};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{AppState, FabricSeed, NodeSeed, ServingSeed};

use crate::common;
use crate::common::ledger_double::RecordingLedger;
use crate::common::{member, spawn_router};

fn empty_model_info(id: u128, name: &str) -> ModelInfo {
    ModelInfo {
        id: ModelId::from_u128(id),
        name: name.into(),
        repo: "test".into(),
        file: format!("{name}.gguf"),
        size_bytes: 1_000_000,
        total_layers: 1,
        architecture: ModelArchitecture::Other,
        available_on: HashMap::new(),
        oicp_capabilities: Default::default(),
        quantization: "Q4_0".into(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    }
}

/// The mesh these tests serve: `self_id` is its only member.
fn build_mesh(self_id: NodeId) -> Mesh {
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "models-http test".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    }
}

/// Build an AppState whose `inference_store` is keyed to `self_id` so
/// any `set_model_info` calls land with self as the origin (= visible
/// to the live-nodes filter in `list_models`).
fn build_state(self_id: NodeId) -> AppState {
    AppState::new(self_id, build_mesh(self_id))
}

/// Build an AppState over the recording double, whose model scan answers
/// `rows` — each `(origin, model)` as a peer's gossiped write would arrive.
fn build_state_with_models(self_id: NodeId, rows: Vec<(NodeId, ModelInfo)>) -> AppState {
    AppState::new_with_seeds(
        self_id,
        build_mesh(self_id),
        None,
        None,
        FabricSeed::default(),
        ServingSeed::default(),
        NodeSeed::default(),
        Arc::new(RecordingLedger::new(self_id).with_models(rows)).seed(),
    )
}

#[tokio::test]
async fn locally_owned_model_appears_in_v1_models_response() {
    let self_id = NodeId::from_u128(0xAAAA_BBBB);
    let state = build_state(self_id);
    // Insert a ModelInfo. `set_model_info` stamps `self_id` as the
    // origin (the constructor at `InferenceStateStore::new` captures
    // self_id from AppState).
    state
        .register_model(empty_model_info(1, "test-local-model"))
        .await
        .unwrap();

    let addr = spawn_router(client_router(state)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .send()
        .await
        .expect("/v1/models must be reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "/v1/models must 200; got {}",
        resp.status()
    );
    let json: serde_json::Value = resp.json().await.unwrap();

    // Envelope shape: `{"object":"list","data":[{...},...]}`.
    assert_eq!(
        json["object"].as_str(),
        Some("list"),
        "envelope must use `object: list`; got: {json}"
    );
    let data = json["data"].as_array().expect("data must be an array");
    let ids: Vec<&str> = data.iter().filter_map(|m| m["id"].as_str()).collect();
    assert!(
        ids.contains(&"test-local-model"),
        "locally-owned model MUST appear in /v1/models data[]. \
         A regression in the origin filter that incorrectly excluded \
         self's own models would brick every client's model picker. \
         Got ids: {ids:?}"
    );
    // The matching entry must carry the OpenAI-shape fields.
    let entry = data
        .iter()
        .find(|m| m["id"].as_str() == Some("test-local-model"))
        .unwrap();
    assert_eq!(
        entry["object"].as_str(),
        Some("model"),
        "per-entry `object` field must be `model` to satisfy \
         openai-client schema validators; got: {entry}"
    );
}

#[tokio::test]
async fn offline_peer_only_model_is_filtered_out_of_v1_models() {
    // The "/v1/models liveness filter" pinned. Mimics a stale
    // gossip entry: a peer wrote a ModelInfo, then went offline.
    // The filter joins ModelInfo.origin against `live_nodes`
    // (online/busy + self); offline → filtered.
    //
    // We can't directly set the ModelInfo's *origin* (that comes
    // from the store's writer NodeId), but we CAN simulate the
    // exact scenario the filter targets: the store port answers a
    // ModelInfo whose origin is an offline peer, and the test AppState
    // is built around a different self_id that doesn't see the peer
    // as online.
    let self_id = NodeId::from_u128(0xC0C0_C0C0);
    let offline_peer_id = NodeId::from_u128(0xDEAD_BEEF);

    // The store port's scan answers the offline peer's write — origin
    // `offline_peer_id` — as gossip would have replicated it.
    let state = build_state_with_models(
        self_id,
        vec![(offline_peer_id, empty_model_info(2, "ghost-model"))],
    );
    // Set up: the offline peer is NOT in the mesh's members, so
    // `live_nodes` will be `{self_id}` only.
    {
        let mesh = state.inner.fabric.mesh.read().await;
        assert!(
            !mesh.members.contains_key(&offline_peer_id),
            "test precondition: the offline peer must NOT be in the \
             mesh's online member set"
        );
    }

    // Sanity: the store sees BOTH the self-owned + peer-owned
    // entries — the filter is the only thing standing between this
    // and the wire.
    let raw_count = state.list_models().await.unwrap().len();
    assert_eq!(
        raw_count, 1,
        "test precondition: with no local-self model registered, \
         only the offline-peer's `ghost-model` is in the raw store; \
         got count={raw_count}"
    );

    let addr = spawn_router(client_router(state)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .send()
        .await
        .expect("/v1/models must be reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let json: serde_json::Value = resp.json().await.unwrap();
    let data = json["data"].as_array().expect("data must be an array");
    let ids: Vec<&str> = data.iter().filter_map(|m| m["id"].as_str()).collect();
    assert!(
        !ids.contains(&"ghost-model"),
        "offline-peer-only model MUST be filtered out of /v1/models — \
         the live-nodes join is what prevents the picker from offering \
         models that can't actually serve. A regression that bypassed \
         the filter would surface as `503 model_not_ready` to every \
         client that picks the filtered model. Got ids: {ids:?}"
    );
}
