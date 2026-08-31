// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end smoke for `POST /v1/embeddings`.
//!
//! Today's coverage of `routes_inference::embeddings` lives entirely
//! at the unit-test layer (response shape, error mapping). Nothing
//! pins that the route actually returns one `EmbeddingData` per
//! input when wired against a real `LocalInferenceService`, or that
//! the "no backend" branch returns the documented 503 rather than
//! a 500 / panic.
//!
//! Two assertions:
//!
//! 1. **Single + batch input shape.** A request with one string
//!    yields `data.len() == 1` and `index == 0`; a request with a
//!    list yields `data.len() == N` with sequential `index` and
//!    every embedding non-empty.
//! 2. **No-backend ⇒ 503 with `no_local_embedding_backend`.** When
//!    `local_inference` is `None`, the route documents this exact
//!    error code; a regression that returned a 500 or fell through
//!    to a different surface would be invisible to callers that
//!    branch on the error code (Sovereign's own desktop bootstrap
//!    does).
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::json;

use commonwealth_api::server::client_router;
use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

use crate::common;
use crate::common::{member, spawn_router, TestProvider};

/// Build an `AppState` with the stub `LocalInferenceService` installed
/// (`with_embed=true`) or without (`with_embed=false`, to pin the
/// no-backend 503).
fn build_app_state(with_embed: bool) -> AppState {
    let self_id = NodeId::from_u128(0xE5E5_E5E5_E5E5_E5E5);
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: commonwealth_core::ids::MeshId::from_u128(7),
        name: "embeddings-test".into(),
        invite_key_hash: [3u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let app_state =
        AppState::new_with_platform_and_engine(self_id, mesh, mesh_store, app_registry, None);
    if !with_embed {
        return app_state;
    }
    // Marker-encoded vector: `embed("foo") = [3.0; 8]`. Lets the
    // test verify per-input ordering survives the fan-out.
    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("stub-embed")
            .with_embed_marker(|input| vec![input.len() as f32; 8]),
    );
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));
    app_state.with_local_inference(adapter)
}

async fn spawn(state: AppState) -> SocketAddr {
    spawn_router(client_router(state)).await
}

#[tokio::test]
async fn single_input_yields_one_embedding_with_index_zero() {
    let addr = spawn(build_app_state(true)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/embeddings"))
        .json(&json!({ "model": "stub-embed", "input": "hello" }))
        .send()
        .await
        .expect("/v1/embeddings reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let data = body["data"].as_array().expect("data must be an array");
    assert_eq!(data.len(), 1, "single input → one embedding row");
    assert_eq!(data[0]["index"].as_u64(), Some(0));
    assert_eq!(data[0]["object"].as_str(), Some("embedding"));
    let vec = data[0]["embedding"]
        .as_array()
        .expect("embedding must be an array");
    assert_eq!(vec.len(), 8, "stub returns 8-dim vectors");
    // Marker check: "hello".len() == 5, so every element should be 5.0.
    assert_eq!(vec[0].as_f64(), Some(5.0));
    assert_eq!(body["model"].as_str(), Some("stub-embed"));
}

#[tokio::test]
async fn batch_input_yields_one_embedding_per_input_with_sequential_index() {
    let addr = spawn(build_app_state(true)).await;
    let inputs = vec!["a", "bb", "ccc"];
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/embeddings"))
        .json(&json!({ "model": "stub-embed", "input": inputs.clone() }))
        .send()
        .await
        .expect("/v1/embeddings reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(
        data.len(),
        inputs.len(),
        "batch of {n} inputs → {n} embedding rows",
        n = inputs.len()
    );
    // index must be sequential, embedding marker must match the
    // input's length — proves per-input ordering survives the fan-out.
    for (i, row) in data.iter().enumerate() {
        assert_eq!(row["index"].as_u64(), Some(i as u64));
        let vec = row["embedding"].as_array().unwrap();
        let marker = vec[0].as_f64().unwrap();
        assert_eq!(
            marker as usize,
            inputs[i].len(),
            "row {i} marker {marker} should equal len(\"{}\") = {}",
            inputs[i],
            inputs[i].len()
        );
    }
}

#[tokio::test]
async fn no_local_inference_backend_returns_503_with_documented_error_code() {
    // `with_embed = false` → `state.inner.local_inference == None`.
    // The route documents this exact error code; the desktop
    // bootstrap branches on it to decide whether to fall back to
    // a peer-served embedding.
    let addr = spawn(build_app_state(false)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/embeddings"))
        .json(&json!({ "model": "anything", "input": "x" }))
        .send()
        .await
        .expect("/v1/embeddings reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "no embed backend must return 503 (not 500 or 404)"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    // OpenAI envelope: { "error": { "message": "...", "type": "...",
    // "code": null } }. The discriminator lives on `.error.type`;
    // `.error.code` is reserved for the OpenAI sub-classification
    // and is null for our daemon-side errors.
    let kind = body["error"]["type"].as_str().unwrap_or("");
    assert_eq!(
        kind, "no_local_embedding_backend",
        "error.type must match the documented contract; got body {body}"
    );
}

#[tokio::test]
async fn empty_batch_returns_400_invalid_request() {
    // OpenAI clients pass `input: []` when they have nothing to
    // embed. The route documents this case as 400 + `invalid_request_error`.
    // Useful to pin so a regression doesn't silently treat empty
    // input as 200-with-empty-data.
    let addr = spawn(build_app_state(true)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/embeddings"))
        .json(&json!({ "model": "stub-embed", "input": Vec::<String>::new() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}
