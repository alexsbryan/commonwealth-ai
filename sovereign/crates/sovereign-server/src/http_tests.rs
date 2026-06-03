//! End-to-end tests for the mobile-facing server surface:
//!   * provenance + citation projection on REST responses,
//!   * `GET /v1/corpora` (graceful empty path),
//!   * the busy guard (`503 + Retry-After`), and
//!   * WebSocket token streaming (`Token`* → `Complete`).
//!
//! `sovereign-server` is a binary crate, so these live in-crate — an
//! integration test under `tests/` couldn't reach the private modules
//! (`crate::routes`, `crate::ws`, `crate::busy`, …).

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::middleware;
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use futures::Stream;
use tower::ServiceExt as _; // for `oneshot`

use sovereign_core::error::Result;
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::stubs::PassthroughRouter;
use sovereign_core::traits::{ApprovalChannel, InferenceProvider, StateStore};
use sovereign_core::ConversationStore; // brings `save_message` into scope
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, InferenceConfig, Message, ProviderCapabilities,
    Role, Speed,
};
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_store::sqlite::SqliteStateStore;

use crate::approval::ServerApprovalChannel;
use crate::auth::AuthState;
use crate::busy::BusyGuard;

const MOCK_BACKEND: &str = "MockLlama.Q8_0 @ peer TestNode";
const MOCK_DELTAS: &[&str] = &["Hello", ", ", "world", "."];

// ─── Streaming mock inference ────────────────────────────────
//
// Streams a fixed set of token deltas and reports a recognisable
// backend id, so provenance assertions have a stable target. `complete`
// covers the background pre-passes (working-memory compaction,
// auto-title) the runtime fires around a turn.

struct StreamingMockInference;

#[async_trait]
impl InferenceProvider for StreamingMockInference {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: MOCK_DELTAS.concat(),
            tokens_used: 4,
            prompt_tokens: 0,
            model_id: MOCK_BACKEND.to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: Some(4),
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let deltas: Vec<Result<String>> = MOCK_DELTAS.iter().map(|s| Ok(s.to_string())).collect();
        Ok(Box::pin(futures::stream::iter(deltas)))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        // The streaming path stamps provenance.inference_backend from
        // this (via `complete_stream_with_id_and_finish`'s default).
        MOCK_BACKEND.to_string()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

// ─── Harness ──────────────────────────────────────────────────

/// Build a Runtime with an in-memory store, PassthroughRouter
/// (SimpleQuery for all), no tools, and a `ServerApprovalChannel`.
/// Returns the concrete store + approval handles the test needs.
fn build_runtime(
    inference: Arc<dyn InferenceProvider>,
) -> (
    Arc<Runtime>,
    Arc<SqliteStateStore>,
    Arc<ServerApprovalChannel>,
) {
    let store_concrete = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let store: Arc<dyn StateStore> = store_concrete.clone();
    let skills = Arc::new(SkillRegistry::new());
    let router: Box<dyn sovereign_core::traits::Router> = Box::new(PassthroughRouter);
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
    let tools = Arc::new(ToolRegistry::new());
    let (approval_chan, _rx) = ServerApprovalChannel::new();
    let approval = Arc::new(approval_chan);

    let runtime = Runtime::new(
        inference,
        router,
        Box::new(planner),
        tools,
        store,
        skills,
        approval.clone() as Arc<dyn ApprovalChannel>,
        InferenceConfig::default(),
    );
    (Arc::new(runtime), store_concrete, approval)
}

/// Build the authed router mirroring `main.rs` (auth disabled → the
/// middleware injects the `default` tenant), with the runtime, approval,
/// and busy-guard extensions every handler under test needs.
fn build_app(
    runtime: Arc<Runtime>,
    approval: Arc<ServerApprovalChannel>,
    busy: BusyGuard,
) -> Router {
    let authed = Router::new()
        .route("/v1/conversations", post(crate::routes::create_conversation))
        .route("/v1/conversations", get(crate::routes::list_conversations))
        .route(
            "/v1/conversations/{id}",
            get(crate::routes::get_conversation),
        )
        .route(
            "/v1/conversations/{id}",
            delete(crate::routes::delete_conversation),
        )
        .route(
            "/v1/conversations/{id}/messages",
            post(crate::routes::send_message),
        )
        .route("/v1/corpora", get(crate::routes::list_corpora))
        .route(
            "/v1/conversations/{id}/stream",
            get(crate::ws::ws_handler),
        )
        .layer(middleware::from_fn(crate::auth::auth_middleware))
        .layer(Extension(AuthState::disabled()));

    authed
        .layer(Extension(runtime))
        .layer(Extension(approval))
        .layer(Extension(busy))
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ─── REST: provenance on a real turn ──────────────────────────

#[tokio::test]
async fn rest_send_message_surfaces_provenance() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(StreamingMockInference);
    let (runtime, _store, approval) = build_runtime(inference);
    let app = build_app(runtime, approval, BusyGuard::new(4, 2));

    let body = serde_json::json!({ "content": "hello there" }).to_string();
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/conversations/conv1/messages")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let json = body_json(resp).await;
    // PassthroughRouter → SimpleQuery; the handler persists provenance
    // with the mock backend id, which the REST projection surfaces.
    assert_eq!(
        json["provenance"]["inference_backend"].as_str(),
        Some(MOCK_BACKEND),
        "provenance.inference_backend should round-trip to the client"
    );
}

// ─── REST: citations projected from persisted metadata ────────

#[tokio::test]
async fn rest_get_conversation_projects_citations() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(StreamingMockInference);
    let (runtime, store, approval) = build_runtime(inference);

    // Seed an assistant message with crafted provenance + retrieved
    // chunks under the default tenant's scoped conversation id. (Auth is
    // disabled → tenant is "default" → scoped id is "default:convX".)
    let meta = serde_json::json!({
        "provenance": {
            "intent": "KnowledgeQuery",
            "inference_backend": "Qwen3.5-9B @ peer BeefyMac",
            "total_latency_ms": 50
        },
        "retrieved_chunks": [
            {"title": "Free Will", "corpus_id": "sep",
             "snippet": "Compatibilism holds that...", "score": 0.91,
             "chunk_id": "sep:free-will:3"}
        ]
    });
    let msg = Message {
        id: "m1".to_string(),
        conversation_id: "default:convX".to_string(),
        role: Role::Assistant,
        content: "answer".to_string(),
        created_at: 1,
        metadata: Some(meta),
        version: 1,
    };
    store.save_message(&msg).await.unwrap();

    let app = build_app(runtime, approval, BusyGuard::new(4, 2));
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/conversations/convX")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let json = body_json(resp).await;
    let m0 = &json["messages"][0];
    assert_eq!(m0["citations"][0]["corpus_id"].as_str(), Some("sep"));
    assert_eq!(
        m0["citations"][0]["chunk_id"].as_str(),
        Some("sep:free-will:3"),
        "citation carries the (corpus_id, chunk_id) handle"
    );
    assert_eq!(m0["citations"][0]["rank"].as_u64(), Some(0));
    assert_eq!(
        m0["provenance"]["inference_backend"].as_str(),
        Some("Qwen3.5-9B @ peer BeefyMac")
    );
}

// ─── REST: corpora endpoint (graceful empty) ──────────────────

#[tokio::test]
async fn corpora_empty_without_engine() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(StreamingMockInference);
    // No corpus engine wired → endpoint must return an empty list, not error.
    let (runtime, _store, approval) = build_runtime(inference);
    let app = build_app(runtime, approval, BusyGuard::new(4, 2));

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/corpora")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let json = body_json(resp).await;
    assert!(
        json["corpora"].as_array().unwrap().is_empty(),
        "no engine → empty corpora list"
    );
}

// ─── REST: busy guard → 503 + Retry-After ─────────────────────

#[tokio::test]
async fn busy_guard_returns_503_with_retry_after() {
    let inference: Arc<dyn InferenceProvider> = Arc::new(StreamingMockInference);
    let (runtime, _store, approval) = build_runtime(inference);

    let busy = BusyGuard::new(1, 7);
    // Occupy the only slot (shares the inner Arc<Semaphore> with the
    // guard moved into the app), so the handler's try_enter() fails.
    let _held = busy.try_enter().expect("first permit granted");

    let app = build_app(runtime, approval, busy);
    let body = serde_json::json!({ "content": "hi" }).to_string();
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/conversations/c/messages")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("7"),
        "busy host advertises Retry-After"
    );
}

// ─── WebSocket: token streaming → Complete ────────────────────

#[tokio::test]
async fn ws_streams_tokens_then_complete() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let inference: Arc<dyn InferenceProvider> = Arc::new(StreamingMockInference);
    let (runtime, _store, approval) = build_runtime(inference);
    let app = build_app(runtime, approval, BusyGuard::new(4, 2));

    // Bind an ephemeral port and serve in the background.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service()).await.unwrap();
    });

    let url = format!("ws://{addr}/v1/conversations/convWS/stream");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .expect("ws connect");

    let send = serde_json::json!({ "type": "message", "data": { "content": "hello" } }).to_string();
    ws.send(WsMessage::Text(send.into())).await.unwrap();

    let mut tokens = 0usize;
    let mut complete_id: Option<String> = None;
    let mut complete_backend: Option<String> = None;

    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(15), ws.next()).await;
        let msg = match next {
            Ok(Some(Ok(m))) => m,
            _ => break, // timeout / closed / error → fall through to assertions
        };
        let txt = match msg {
            WsMessage::Text(t) => t.as_str().to_owned(),
            WsMessage::Close(_) => break,
            _ => continue,
        };
        let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
        match v["type"].as_str() {
            Some("token") => tokens += 1,
            Some("complete") => {
                complete_id = v["data"]["message_id"].as_str().map(str::to_string);
                complete_backend = v["data"]["provenance"]["inference_backend"]
                    .as_str()
                    .map(str::to_string);
                break;
            }
            Some("stream_error") => panic!("unexpected stream_error frame: {txt}"),
            _ => {}
        }
    }

    assert!(tokens >= 1, "expected at least one token frame, got {tokens}");
    assert!(complete_id.is_some(), "expected a terminal complete frame");
    assert_eq!(
        complete_backend.as_deref(),
        Some(MOCK_BACKEND),
        "complete frame carries provenance with the serving backend"
    );
}
