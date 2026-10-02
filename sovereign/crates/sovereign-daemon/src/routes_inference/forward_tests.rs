// SPDX-License-Identifier: AGPL-3.0-or-later
//! The forwarding half of `chat_completions` (Priorities 1-4): with no
//! in-process `local_inference`, a turn goes to the llama-server address the
//! ledger holds for the model it resolves to. a546a456b stopped the harness
//! tests that drove these through a simulated mesh; these are their
//! successors at the route's owner (pb-distribution-o3-tests).

use super::*;
use crate::ledger_port::{InferencePlan, ShardPlan};
use crate::state::test_app_state;
use axum::body::Body;
use axum::http::Request;
use kernel_types::{ModelId, NodeId};
use oicp_types::model_catalog::{ModelArchitecture, ModelInfo};
use oicp_types::{Capability, CapabilityProfile};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

/// A llama-server stand-in: answers every chat with one assistant message
/// and counts what it served.
async fn backend() -> (String, Arc<AtomicUsize>) {
    let served = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&served);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let count = Arc::clone(&count);
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "from the backend" },
                        "finish_reason": "stop"
                    }]
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr.to_string(), served)
}

fn model(id: u128, name: &str, caps: &[(Capability, u8)]) -> ModelInfo {
    let mut profile = CapabilityProfile::default();
    for &(c, level) in caps {
        profile.insert(c, level);
    }
    ModelInfo {
        id: ModelId::from_u128(id),
        name: name.into(),
        repo: format!("test/{name}"),
        file: format!("{name}.gguf"),
        size_bytes: 17 * 1_073_741_824,
        total_layers: 64,
        architecture: ModelArchitecture::Qwen,
        available_on: Default::default(),
        oicp_capabilities: profile,
        quantization: "Q4_K_M".into(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    }
}

fn coder() -> ModelInfo {
    model(
        1,
        "qwen3-coder-30b",
        &[
            (Capability::Code, 4),
            (Capability::Instruction, 3),
            (Capability::General, 2),
        ],
    )
}

fn general() -> ModelInfo {
    model(
        2,
        "qwen3-30b",
        &[
            (Capability::General, 3),
            (Capability::Analysis, 3),
            (Capability::Creative, 3),
            (Capability::Code, 2),
        ],
    )
}

/// Register `models` in plan order (the first is the default), each served
/// at its address.
async fn node(models: Vec<(ModelInfo, String)>) -> AppState {
    let state = test_app_state();
    let mut plan = InferencePlan::default();
    for (m, addr) in models {
        let id = m.id;
        state.register_model(m).await.unwrap();
        state.set_llama_server_address(id, addr).await.unwrap();
        plan.model_plans.push(ShardPlan {
            model: id,
            entry_node: NodeId::from_u128(1),
            assignments: Vec::new(),
            estimated_tokens_per_sec: 40.0,
            estimated_ttft_ms: 1000,
        });
    }
    state.set_inference_plan(&plan).await.unwrap();
    state
}

async fn chat(
    state: &AppState,
    body: serde_json::Value,
) -> (StatusCode, HeaderMap, serde_json::Value) {
    let resp = crate::server::mock_router(state.clone())
        .oneshot(
            Request::post("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or_default(),
    )
}

/// Successor of `inference_e2e_with_mock_llama_server`.
#[tokio::test]
async fn a_turn_is_forwarded_to_the_models_backend_and_its_answer_returned() {
    let (addr, served) = backend().await;
    let state = node(vec![(general(), addr)]).await;
    let (status, _, body) = chat(
        &state,
        serde_json::json!({ "messages": [{ "role": "user", "content": "Hello" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "from the backend");
    assert_eq!(served.load(Ordering::SeqCst), 1);
}

/// Successor of `inference_503_retry_after_on_backend_failure`: a backend
/// that does not answer is a 503 the client may retry, never a 502.
#[tokio::test]
async fn an_unreachable_backend_is_a_503_with_retry_after() {
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let state = node(vec![(general(), dead)]).await;
    let (status, headers, body) = chat(
        &state,
        serde_json::json!({ "messages": [{ "role": "user", "content": "Hello" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        headers.contains_key("retry-after"),
        "a backend failure must say when to retry: {headers:?}"
    );
    assert!(
        body["error"]["type"]
            .as_str()
            .is_some_and(|t| t.contains("unavailable")),
        "the error type names the unavailability: {body}"
    );
}

/// Successor of `oicp_routing_selects_correct_model`.
#[tokio::test]
async fn an_oicp_hint_routes_to_the_model_whose_claims_match() {
    let ((a_coder, coder_served), (a_general, general_served)) = (backend().await, backend().await);
    let state = node(vec![(coder(), a_coder), (general(), a_general)]).await;
    let ask = |hint: &str| {
        serde_json::json!({
            "messages": [{ "role": "user", "content": "Hello" }],
            "oicp": {
                "oicp_version": "0.3.0",
                "capability_hint": hint,
                "latency_class": "normal",
                "privacy": { "sharding": "mesh_allowed" }
            }
        })
    };
    let (status, _, body) = chat(&state, ask("code")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            coder_served.load(Ordering::SeqCst),
            general_served.load(Ordering::SeqCst)
        ),
        (1, 0),
        "a code hint goes to the coder"
    );
    let (status, _, body) = chat(&state, ask("general")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            coder_served.load(Ordering::SeqCst),
            general_served.load(Ordering::SeqCst)
        ),
        (1, 1),
        "a general hint goes to the general model"
    );
}

/// Successor of `omo_model_alias_routes_to_coding_model`: a client's model
/// name the alias table knows becomes OICP requirements.
#[tokio::test]
async fn an_aliased_model_name_routes_by_the_aliases_requirements() {
    let ((a_coder, coder_served), (a_general, general_served)) = (backend().await, backend().await);
    let state = node(vec![(general(), a_general), (coder(), a_coder)]).await;
    let named = |model: &str| serde_json::json!({ "model": model, "messages": [{ "role": "user", "content": "Hello" }] });
    let (status, _, body) = chat(&state, named("gpt-5.3-codex")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            coder_served.load(Ordering::SeqCst),
            general_served.load(Ordering::SeqCst)
        ),
        (1, 0),
        "a codex alias goes to the coder, not the default"
    );
    let (status, _, body) = chat(&state, named("claude-opus-4-6")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            coder_served.load(Ordering::SeqCst),
            general_served.load(Ordering::SeqCst)
        ),
        (1, 1),
        "an opus alias goes to the general model"
    );
}

/// Successor of `unknown_model_name_falls_through_to_default`: on this
/// forwarding path a name nothing matches is served by the plan's default.
#[tokio::test]
async fn an_unknown_model_name_falls_through_to_the_default_model() {
    let ((a_coder, coder_served), (a_general, general_served)) = (backend().await, backend().await);
    let state = node(vec![(general(), a_general), (coder(), a_coder)]).await;
    let (status, _, body) = chat(
        &state,
        serde_json::json!({
            "model": "totally-unknown-model-v99",
            "messages": [{ "role": "user", "content": "Hello" }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            general_served.load(Ordering::SeqCst),
            coder_served.load(Ordering::SeqCst)
        ),
        (1, 0),
        "the default is the plan's first model"
    );
}
