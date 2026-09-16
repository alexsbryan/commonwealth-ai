// SPDX-License-Identifier: AGPL-3.0-or-later
//! `mesh_admin` route tests — the model load/unload/warmup surface and the
//! join handshake.
//!
//! Split out of `mesh_admin.rs` at REVIEW-audit-9 when the AppState repoint
//! (`state.inner.<part>` paths) pushed the file past arch-gate's slack; the
//! module path is unchanged (`#[path]` only moves the file).

use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode as HttpStatus};
use axum::routing::post;
use axum::Router;
use tower::ServiceExt;

use crate::state::{test_app_state, test_app_state_with_inference};

fn activity_router() -> (AppState, Router) {
    let state = test_app_state();
    let app = Router::new()
        .route("/internal/node/activity", post(node_activity))
        .with_state(state.clone());
    (state, app)
}

async fn post_activity(app: Router, level: &str, reason: &str) -> HttpStatus {
    let body = serde_json::json!({ "level": level, "reason": reason }).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/node/activity")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    response.status()
}

#[tokio::test]
async fn hot_level_returns_204_no_content() {
    let (_, app) = activity_router();
    let status = post_activity(app, "hot", "tests_running").await;
    assert_eq!(status, HttpStatus::NO_CONTENT);
}

#[tokio::test]
async fn hot_level_sets_availability_to_020() {
    let (state, app) = activity_router();
    post_activity(app, "hot", "tests_running").await;
    let val = *state
        .inner
        .serving
        .local_inference_availability
        .read()
        .await;
    assert!(
        (val - 0.20).abs() < 1e-6,
        "hot must set availability to 0.20, got {val}"
    );
}

#[tokio::test]
async fn warm_level_sets_availability_to_065() {
    let (state, app) = activity_router();
    post_activity(app, "warm", "recent_edits").await;
    let val = *state
        .inner
        .serving
        .local_inference_availability
        .read()
        .await;
    assert!(
        (val - 0.65).abs() < 1e-6,
        "warm must set availability to 0.65, got {val}"
    );
}

#[tokio::test]
async fn cool_level_sets_availability_to_085() {
    let (state, app) = activity_router();
    post_activity(app, "cool", "settling").await;
    let val = *state
        .inner
        .serving
        .local_inference_availability
        .read()
        .await;
    assert!(
        (val - 0.85).abs() < 1e-6,
        "cool must set availability to 0.85, got {val}"
    );
}

#[tokio::test]
async fn idle_level_sets_availability_to_100() {
    // Start hot, then go idle to verify full round-trip.
    let (state, app) = activity_router();
    post_activity(app.clone(), "hot", "start").await;
    post_activity(app, "idle", "long_pause").await;
    let val = *state
        .inner
        .serving
        .local_inference_availability
        .read()
        .await;
    assert!(
        (val - 1.00).abs() < 1e-6,
        "idle must set availability to 1.00, got {val}"
    );
}

#[tokio::test]
async fn unknown_level_defaults_to_idle() {
    let (state, app) = activity_router();
    post_activity(app, "turbo", "unknown_level").await;
    let val = *state
        .inner
        .serving
        .local_inference_availability
        .read()
        .await;
    assert!(
        (val - 1.00).abs() < 1e-6,
        "unknown level must default to 1.00, got {val}"
    );
}

// ── Runtime model slot management tests ──────────────────

/// Stub `LocalInferenceService` that records every load/unload
/// request and replays canned answers. Inference methods
/// (`chat_completion`, `embed`) are stubbed to return errors
/// since these tests only exercise the slot-management surface.
struct StubLocalInference {
    load_calls: std::sync::Mutex<Vec<(String, std::path::PathBuf, u32)>>,
    unload_calls: std::sync::Mutex<Vec<String>>,
    load_response: Result<String, String>,
    inventory: Vec<(String, String)>,
}

impl StubLocalInference {
    fn new(load_response: Result<String, String>, inventory: Vec<(String, String)>) -> Self {
        Self {
            load_calls: std::sync::Mutex::new(Vec::new()),
            unload_calls: std::sync::Mutex::new(Vec::new()),
            load_response,
            inventory,
        }
    }
}

#[async_trait::async_trait]
impl sovereign_core::traits::InferenceProvider for StubLocalInference {
    async fn complete(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<sovereign_core::types::CompletionResponse> {
        Err(sovereign_core::error::Error::Inference("stub".into()))
    }

    async fn complete_stream(
        &self,
        _request: &sovereign_core::types::CompletionRequest,
    ) -> sovereign_core::error::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = sovereign_core::error::Result<String>> + Send>,
        >,
    > {
        Err(sovereign_core::error::Error::Inference("stub".into()))
    }

    async fn embed(&self, _input: &str) -> sovereign_core::error::Result<Vec<f32>> {
        Err(sovereign_core::error::Error::Inference("stub".into()))
    }

    fn capabilities(&self) -> sovereign_core::types::ProviderCapabilities {
        unimplemented!()
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> sovereign_core::error::Result<String> {
        self.load_calls
            .lock()
            .unwrap()
            .push((slot_name.clone(), path.clone(), context_size));
        self.load_response
            .clone()
            .map_err(sovereign_core::error::Error::Inference)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> sovereign_core::error::Result<Option<String>> {
        self.unload_calls.lock().unwrap().push(slot_name.into());
        // Stub returns Some(...) for any slot in inventory, None
        // otherwise.
        Ok(self
            .inventory
            .iter()
            .find(|(name, _)| name == slot_name)
            .map(|(_, mid)| mid.clone()))
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.inventory.clone()
    }
}

#[async_trait::async_trait]
impl crate::state::LocalInferenceService for StubLocalInference {
    async fn chat_completion(
        &self,
        _request: crate::openai_types::ChatCompletionRequest,
    ) -> Result<crate::openai_types::ChatCompletionResponse, crate::state::LocalInferenceError>
    {
        Err("stub".into())
    }

    async fn chat_completion_stream(
        &self,
        _request: crate::openai_types::ChatCompletionRequest,
    ) -> Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = crate::openai_types::StreamFrame> + Send>>,
        crate::state::LocalInferenceError,
    > {
        Err("stub".into())
    }

    fn provider_manifest(&self) -> Option<oicp_types::ProviderManifest> {
        None
    }
}

fn models_router(stub: Arc<StubLocalInference>) -> Router {
    let state = test_app_state_with_inference(stub);
    Router::new()
        .route("/internal/models/load", post(models_load))
        .route("/internal/models/unload", post(models_unload))
        .route(
            "/internal/models/inventory",
            axum::routing::get(models_inventory),
        )
        .with_state(state)
}

#[tokio::test]
async fn models_load_returns_503_when_local_inference_absent() {
    // No inference service in the seed → local_inference is None.
    let state = test_app_state();
    let app = Router::new()
        .route("/internal/models/load", post(models_load))
        .with_state(state);
    let body = serde_json::json!({
        "slot_name": "bulk",
        "path": "/m/x.gguf"
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/load")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn models_load_forwards_to_provider_and_returns_model_id() {
    let stub = Arc::new(StubLocalInference::new(
        Ok("Qwen3.5-9B.Q8_0".into()),
        vec![],
    ));
    let app = models_router(Arc::clone(&stub));
    let body = serde_json::json!({
        "slot_name": "bulk",
        "path": "/m/qwen.gguf",
        "context_size": 32768
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/load")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::OK);

    // Verify the stub recorded the call with the request fields
    // forwarded verbatim.
    let calls = stub.load_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "bulk");
    assert_eq!(calls[0].1, std::path::PathBuf::from("/m/qwen.gguf"));
    assert_eq!(calls[0].2, 32768);
}

#[tokio::test]
async fn models_load_default_context_size_is_16384() {
    // Operators who omit `context_size` get the daemon-wide
    // default. Lock the contract so a future change is visible.
    let stub = Arc::new(StubLocalInference::new(Ok("m".into()), vec![]));
    let app = models_router(Arc::clone(&stub));
    let body = serde_json::json!({
        "slot_name": "bulk",
        "path": "/m/x.gguf"
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/load")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let _ = app.oneshot(req).await.unwrap();
    let calls = stub.load_calls.lock().unwrap();
    assert_eq!(calls[0].2, 16_384);
}

#[tokio::test]
async fn models_load_returns_400_on_provider_error() {
    // Stub provider rejects (e.g. a remote inference backend that
    // doesn't support runtime slot mutation). Handler surfaces
    // the error verbatim.
    let stub = Arc::new(StubLocalInference::new(Err("bad path".into()), vec![]));
    let app = models_router(stub);
    let body = serde_json::json!({
        "slot_name": "bulk",
        "path": "/m/x.gguf"
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/load")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::BAD_REQUEST);
}

#[tokio::test]
async fn models_unload_returns_model_id_on_match() {
    let stub = Arc::new(StubLocalInference::new(
        Ok("ignored".into()),
        vec![("bulk".into(), "Qwen3.5-9B.Q8_0".into())],
    ));
    let app = models_router(Arc::clone(&stub));
    let body = serde_json::json!({"slot_name": "bulk"}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/unload")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["slot_name"], "bulk");
    assert_eq!(body["model_id"], "Qwen3.5-9B.Q8_0");
}

#[tokio::test]
async fn models_unload_returns_null_model_id_when_slot_absent() {
    let stub = Arc::new(StubLocalInference::new(Ok("ignored".into()), vec![]));
    let app = models_router(stub);
    let body = serde_json::json!({"slot_name": "missing"}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/unload")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // serde_json renders `Option::None` as JSON null.
    assert!(body["model_id"].is_null());
}

#[tokio::test]
async fn models_inventory_returns_loaded_extras() {
    let stub = Arc::new(StubLocalInference::new(
        Ok("ignored".into()),
        vec![
            ("bulk".into(), "Qwen3.5-9B.Q8_0".into()),
            ("reasoning".into(), "Qwopus3.5-27B-v3-Q6_K".into()),
        ],
    ));
    let app = models_router(stub);
    let req = Request::builder()
        .method("GET")
        .uri("/internal/models/inventory")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let extras = body["extras"].as_array().unwrap();
    assert_eq!(extras.len(), 2);
}

#[tokio::test]
async fn models_load_registers_in_inference_store_for_v1_models() {
    // After load_extra_slot succeeds, the handler also writes a
    // ModelInfo into inference_store so /v1/models advertises
    // the new slot. Without this, clients couldn't see the new
    // entry until the next daemon restart even though routing
    // would have worked.
    let stub = Arc::new(StubLocalInference::new(Ok("test-model".into()), vec![]));
    let state = test_app_state_with_inference(Arc::clone(&stub) as Arc<_>);
    let app = Router::new()
        .route("/internal/models/load", post(models_load))
        .with_state(state.clone());
    let body = serde_json::json!({
        "slot_name": "bulk",
        "path": "/m/test.gguf"
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/load")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::OK);

    // The store should now contain a ModelInfo whose name is
    // the model_id returned by the provider.
    let models = state.inner.serving.inference_store.list_models();
    assert!(
        models.values().any(|m| m.name == "test-model"),
        "post-load: expected `test-model` in inference_store; got {:?}",
        models.values().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn models_unload_drops_from_inference_store() {
    // Pre-seed a store entry, then unload, assert it's gone.
    let stub = Arc::new(StubLocalInference::new(
        Ok("ignored".into()),
        vec![("bulk".into(), "Qwen3.5-9B.Q8_0".into())],
    ));
    let state = test_app_state_with_inference(Arc::clone(&stub) as Arc<_>);
    // Register a fake entry the way `models_load` would so we
    // can verify removal.
    register_extras_in_store(
        &state,
        "bulk",
        std::path::Path::new("/m/qwen.gguf"),
        "Qwen3.5-9B.Q8_0",
    );
    assert!(state
        .inner
        .serving
        .inference_store
        .list_models()
        .values()
        .any(|m| m.name == "Qwen3.5-9B.Q8_0"));

    let app = Router::new()
        .route("/internal/models/unload", post(models_unload))
        .with_state(state.clone());
    let body = serde_json::json!({"slot_name": "bulk"}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/internal/models/unload")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::OK);

    let models = state.inner.serving.inference_store.list_models();
    assert!(
        !models.values().any(|m| m.name == "Qwen3.5-9B.Q8_0"),
        "post-unload: model_id should no longer be in store; got {:?}",
        models.values().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn models_inventory_empty_when_local_inference_absent() {
    // No local inference → empty inventory (not an error). This
    // keeps the GET route monitor-friendly: a recurring poll that
    // 200s with an empty list is easier to operate than one that
    // alternates 503/200 across daemon configurations.
    let state = test_app_state();
    let app = Router::new()
        .route(
            "/internal/models/inventory",
            axum::routing::get(models_inventory),
        )
        .with_state(state);
    let req = Request::builder()
        .method("GET")
        .uri("/internal/models/inventory")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), HttpStatus::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["extras"].as_array().unwrap().len(), 0);
}

// Storage-budget HTTP round-trip + helper tests live as
// integration tests at `tests/storage_budget_route.rs` so they
// can run independently of the (currently-broken) lib test
// target.
