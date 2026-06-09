// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/v1/responses` adapter integration — wire-format translation
//! over the existing `/v1/chat/completions` pipeline.
//!
//! The Responses API is what `codex` and the OpenAI agents libraries
//! speak after dropping `wire_api="chat"` in May 2026. `routes_responses`
//! is a translation layer in front of the unmodified `chat_completions`
//! handler. Unit-level translation tests live in
//! `responses_types::tests` (parser shape) and `routes_responses`
//! (translator logic) — what's not pinned is the end-to-end shape
//! through a real HTTP listener with a real provider on the back side.
//!
//! Three assertions:
//!
//! 1. **Non-streaming happy path.** `input` text + a configured local
//!    provider → 200 with the canonical `response` object shape (id,
//!    object, status=completed, output[0]=Message with output_text).
//!    Regression target: any breakage in `translate_non_streaming_response`
//!    or `build_non_streaming_response` that drops the output array or
//!    inverts the role.
//! 2. **`previous_response_id` is rejected.** The adapter doesn't
//!    implement server-side state; sending the field must 400 (not
//!    silently drop). Codex tolerates the 400 by re-sending the full
//!    conversation, so silently dropping context would corrupt the
//!    conversation chain without any caller seeing an error.
//! 3. **Streaming surfaces `response.completed`.** The SSE stream
//!    must terminate with the `response.completed` event (per spec).
//!    Regression target: `translate_streaming_response`'s state
//!    machine forgetting to emit the terminal event when the inner
//!    chat.completions stream finishes cleanly.
use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_api::server::client_router;
use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

mod common;
use common::{member, spawn_router, TestProvider};

/// Build an `AppState` with `TestProvider` wired as the local
/// inference adapter. The provider returns `complete_text` for
/// non-streaming and emits a single chunk for streaming.
fn build_state() -> AppState {
    let self_id = NodeId::from_u128(0x9999_8888_7777_6666);
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "responses-test".into(),
        join_key_hash: [0x55; 32],
        members,
        peers: vec![],
    };
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let state =
        AppState::new_with_platform_and_engine(self_id, mesh, mesh_store, app_registry, None);
    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("responses-stub")
            .with_complete_text("hello from responses adapter")
            .with_stream_chunks(vec!["hello ".to_string(), "world".to_string()]),
    );
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));
    state.with_local_inference(adapter)
}

#[tokio::test]
async fn non_streaming_input_text_returns_canonical_response_shape() {
    let addr = spawn_router(client_router(build_state())).await;

    let body = serde_json::json!({
        "model": "responses-stub",
        "input": "ping",
        "stream": false,
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/responses"))
        .json(&body)
        .send()
        .await
        .expect("/v1/responses must be reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "non-streaming responses request should 200; \
         a 503 here means the chat.completions pipeline didn't see \
         the translated request"
    );
    let json: serde_json::Value = resp.json().await.unwrap();

    // Canonical envelope: id + object + status="completed" + model
    // (echoes what the caller asked for) + output array.
    assert!(
        json["id"].as_str().unwrap_or("").starts_with("resp_"),
        "id must start with `resp_` per Responses spec; got: {json}"
    );
    assert_eq!(
        json["object"].as_str(),
        Some("response"),
        "object discriminator must be `response`; got: {json}"
    );
    assert_eq!(
        json["status"].as_str(),
        Some("completed"),
        "status on a non-streaming success must be `completed`; got: {json}"
    );
    assert_eq!(
        json["model"].as_str(),
        Some("responses-stub"),
        "model must echo the request's model label; got: {json}"
    );

    // Output must be an array with at least one Message containing
    // the translated text. Regression here would mean the adapter
    // didn't pull content out of chat.completions choices[0].message.content.
    let output = json["output"].as_array().expect("output must be an array");
    assert!(
        !output.is_empty(),
        "output must contain at least one item; got: {json}"
    );

    // First output item is a message with our text. Don't pin role
    // capitalization — that's an enum on the wire.
    let msg = &output[0];
    assert_eq!(
        msg["type"].as_str(),
        Some("message"),
        "first output item must be type=message; got: {msg}"
    );
    let content = msg["content"]
        .as_array()
        .expect("message.content must be an array");
    assert!(
        !content.is_empty(),
        "message.content must have at least one part"
    );
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("hello from responses adapter"),
        "first content part's text must carry the provider's output verbatim; \
         got text={text:?}, full message={msg}"
    );
}

#[tokio::test]
async fn previous_response_id_rejected_with_400_not_silent_drop() {
    let addr = spawn_router(client_router(build_state())).await;

    let body = serde_json::json!({
        "model": "responses-stub",
        "input": "continued conversation",
        "previous_response_id": "resp_abc123",
        "stream": false,
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/responses"))
        .json(&body)
        .send()
        .await
        .expect("/v1/responses must be reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "previous_response_id MUST be rejected with 400 — silent drop \
         would corrupt the conversation chain since codex relies on the \
         400 to know it must re-send full history"
    );
    let json: serde_json::Value = resp.json().await.unwrap_or_default();
    // The error body uses an OpenAI-style envelope; check the message
    // mentions `previous_response_id` so the caller can diagnose.
    let msg = json["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("previous_response_id"),
        "error message must name the offending field for diagnostics; \
         got: {json}"
    );
}

#[tokio::test]
async fn streaming_sse_terminates_with_response_completed_event() {
    let addr = spawn_router(client_router(build_state())).await;

    let body = serde_json::json!({
        "model": "responses-stub",
        "input": "stream this",
        "stream": true,
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/responses"))
        .json(&body)
        .send()
        .await
        .expect("/v1/responses must be reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "streaming responses request should 200; got: {:?}",
        resp.status()
    );

    // Drain the SSE body. Reqwest's `text()` reads to EOF, which the
    // chat.completions stream provides via `[DONE]`. Adapter buffers
    // events per chat.completions chunk; we just want to see the
    // terminal `response.completed` event in the byte stream.
    let body_text = resp.text().await.expect("stream body must be readable");

    // SSE event lines look like `event: response.<kind>\ndata: {...}\n\n`.
    // Don't parse them — just assert the terminal `response.completed`
    // event name appears at least once. A regression that drops the
    // terminator (the most common bug) would fail this assertion.
    assert!(
        body_text.contains("event: response.completed"),
        "streaming response MUST emit `event: response.completed` as the \
         terminal frame — codex (and any well-behaved Responses client) \
         keeps the connection alive until this event arrives. \
         Missing it = the client hangs waiting forever. \
         Body snippet (first 800 chars): {:?}",
        &body_text[..body_text.len().min(800)]
    );

    // Sanity: we also expect `response.created` near the start —
    // again, just a name check, not full parse.
    assert!(
        body_text.contains("event: response.created"),
        "streaming response MUST emit `event: response.created` as the \
         opening event. Missing it = the client never transitions out of \
         its `awaiting_response` state. Body snippet: {:?}",
        &body_text[..body_text.len().min(400)]
    );
}
