// SPDX-License-Identifier: AGPL-3.0-or-later
//! Streaming `finish_reason` end-to-end test.
//!
//! The typed `StreamFrame` surface on `InferenceProvider`
//! (`complete_stream_with_finish`) was added specifically to fix a
//! silent-truncation bug: pre-fix, every truncated completion looked
//! identical to a clean `stop` on the wire because the legacy
//! `Stream<Item = Result<String>>` couldn't carry the terminal
//! reason. The fix puts `Finish { reason, usage }` on the typed
//! stream and the SSE handler at
//! `routes_inference::serve_local_stream` renders the final chunk
//! with `"finish_reason": reason.as_openai_str()`.
//!
//! Unit tests in `inference_adapter::translate_stream_frame` pin
//! the per-variant translation. **What's not pinned:** a
//! `complete_stream_with_finish` that overrides to `Length` actually
//! produces an SSE chunk reading `"finish_reason":"length"` — i.e.
//! the typed surface flows end-to-end through `chat_completion_stream`
//! → `translate_stream_frame` → the SSE renderer → the wire.
//!
//! A regression that re-introduced the legacy default (synthesise
//! `Stop`) would slip past every unit test but be caught here.
//!
//! Cases:
//!
//! 1. **Length-truncated** → final chunk `"finish_reason":"length"`.
//! 2. **Content-filter** → final chunk `"finish_reason":"content_filter"`.
//! 3. **Clean stop** (default-impl path, legacy provider) → final
//!    chunk `"finish_reason":"stop"`. Negative control proving the
//!    test isn't always matching "length".
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::json;

use commonwealth_api::server::client_router;
use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{FinishReason, StreamFrame};
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

mod common;
use common::{member, spawn_router, TestProvider};

/// Build a sequence of typed frames terminating in a specific
/// `FinishReason` — for plugging into `TestProvider::with_typed_frames`
/// when a test wants a non-Stop terminator on the wire.
fn frames_ending_in(prelude: Vec<&str>, reason: FinishReason) -> Vec<StreamFrame> {
    let mut out: Vec<StreamFrame> = prelude
        .into_iter()
        .map(|c| StreamFrame::Token(c.to_string()))
        .collect();
    out.push(StreamFrame::Finish {
        reason,
        usage: None,
    });
    out
}

fn build_state(provider: Arc<dyn InferenceProvider>) -> AppState {
    let self_id = NodeId::from_u128(0xCAFE_CAFE_CAFE_CAFE);
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "finish-reason-test".into(),
        join_key_hash: [4u8; 32],
        members,
        peers: vec![],
    };
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let state =
        AppState::new_with_platform_and_engine(self_id, mesh, mesh_store, app_registry, None);
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));
    state.with_local_inference(adapter)
}

async fn spawn(state: AppState) -> SocketAddr {
    spawn_router(client_router(state)).await
}

/// Drain the SSE body into the list of parsed event-data JSON values.
/// Each `data: { ... }` is captured; the trailing `data: [DONE]`
/// sentinel is dropped because it isn't JSON.
async fn collect_sse_payloads(resp: reqwest::Response) -> Vec<serde_json::Value> {
    let body = resp.text().await.expect("body");
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|s| !s.trim().is_empty() && s.trim() != "[DONE]")
        .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .collect()
}

/// Find the last chunk whose `choices[0].finish_reason` is non-null
/// and return that reason. Returns `None` if no terminal chunk found.
fn terminal_finish_reason(payloads: &[serde_json::Value]) -> Option<String> {
    for p in payloads.iter().rev() {
        let fr = &p["choices"][0]["finish_reason"];
        if !fr.is_null() {
            return fr.as_str().map(String::from);
        }
    }
    None
}

#[tokio::test]
async fn length_truncation_surfaces_length_on_final_chunk() {
    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("fixed-finish")
            .with_typed_frames(frames_ending_in(
                vec!["partial ", "output ", "clipped"],
                FinishReason::Length,
            )),
    );
    let addr = spawn(build_state(provider)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({
            "model": "fixed-finish",
            "messages": [{"role": "user", "content": "go"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("/v1/chat/completions reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let payloads = collect_sse_payloads(resp).await;
    assert!(
        !payloads.is_empty(),
        "stream must produce at least one event"
    );
    let reason = terminal_finish_reason(&payloads)
        .expect("stream must include a terminal chunk with a non-null finish_reason");
    assert_eq!(
        reason, "length",
        "length-truncated stream must surface finish_reason=length on the SSE \
         wire; a `stop` here means typed-stream framing was lost end-to-end. \
         Final payloads: {payloads:?}"
    );
}

#[tokio::test]
async fn content_filter_truncation_surfaces_content_filter_on_final_chunk() {
    // Sister assertion: a different non-Stop FinishReason flows through
    // the same wiring. Catches a regression that hard-coded `length`
    // for any non-Stop variant.
    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("fixed-finish")
            .with_typed_frames(frames_ending_in(
                vec!["safe prelude "],
                FinishReason::ContentFilter,
            )),
    );
    let addr = spawn(build_state(provider)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({
            "model": "fixed-finish",
            "messages": [{"role": "user", "content": "anything"}],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let payloads = collect_sse_payloads(resp).await;
    let reason = terminal_finish_reason(&payloads).expect("terminal chunk");
    assert_eq!(reason, "content_filter");
}

#[tokio::test]
async fn legacy_provider_default_impl_surfaces_stop() {
    // Negative control: a provider that ONLY implements the legacy
    // `Result<String>` surface routes through the trait's default
    // `complete_stream_with_finish` impl, which appends a synthetic
    // `Stop`. The wire chunk must say `finish_reason=stop` — both
    // to prove the legacy path still works AND to prove the
    // assertions above aren't somehow always returning their
    // expected value.
    // No `with_typed_frames` — the provider falls through to
    // `complete_stream` and the default-impl synthesis tail
    // appends `Stop`.
    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("legacy-stream")
            .with_stream_chunks(vec!["clean ".into(), "ending".into()]),
    );
    let addr = spawn(build_state(provider)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({
            "model": "legacy-stream",
            "messages": [{"role": "user", "content": "fine"}],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let payloads = collect_sse_payloads(resp).await;
    let reason = terminal_finish_reason(&payloads).expect("terminal chunk");
    assert_eq!(
        reason, "stop",
        "legacy provider's default-impl path must surface finish_reason=stop"
    );
}
