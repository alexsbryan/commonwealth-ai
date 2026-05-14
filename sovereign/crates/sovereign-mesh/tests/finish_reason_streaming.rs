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
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use serde_json::json;

use commonwealth_api::server::client_router;
use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::capabilities::{
    AvailableResources, HardwareProfile, NodeCapabilities,
};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_state::MeshStore;
use sovereign_core::error::Result as SovResult;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderCapabilities,
    Speed, StreamFrame,
};
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

/// `InferenceProvider` that emits a fixed sequence of typed frames
/// terminating in a configurable `FinishReason`. Tests construct a
/// new provider per case.
struct FixedFinishProvider {
    chunks: Vec<String>,
    terminal: FinishReason,
}

#[async_trait]
impl InferenceProvider for FixedFinishProvider {
    async fn complete(&self, _: &CompletionRequest) -> SovResult<CompletionResponse> {
        unreachable!("streaming test does not call complete()")
    }

    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        // The legacy surface is only called when `complete_stream_with_finish`
        // is NOT overridden. For the negative-control test we use this path
        // (FixedFinishProvider isn't constructed in that case — see
        // LegacyStreamProvider below).
        unreachable!("FixedFinishProvider routes through complete_stream_with_finish")
    }

    async fn complete_stream_with_finish(
        &self,
        _: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        let mut frames: Vec<StreamFrame> = self
            .chunks
            .iter()
            .map(|c| StreamFrame::Token(c.clone()))
            .collect();
        frames.push(StreamFrame::Finish {
            reason: self.terminal.clone(),
            usage: None,
        });
        Ok(Box::pin(futures::stream::iter(frames)))
    }

    async fn embed(&self, _: &str) -> SovResult<Vec<f32>> {
        unreachable!("streaming test does not call embed()")
    }

    fn model_id_for(&self, _: Speed) -> String {
        "fixed-finish".into()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4_096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: sovereign_core::types::Depth::Moderate,
        }
    }
}

/// Legacy-style `InferenceProvider` that ONLY implements the
/// `Result<String>` surface — it never overrides
/// `complete_stream_with_finish`. The trait's default impl
/// appends a synthetic `Stop`. Used for the negative-control test.
struct LegacyStreamProvider {
    chunks: Vec<String>,
}

#[async_trait]
impl InferenceProvider for LegacyStreamProvider {
    async fn complete(&self, _: &CompletionRequest) -> SovResult<CompletionResponse> {
        unreachable!()
    }

    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        let items: Vec<SovResult<String>> =
            self.chunks.iter().cloned().map(Ok).collect();
        Ok(Box::pin(futures::stream::iter(items)))
    }

    async fn embed(&self, _: &str) -> SovResult<Vec<f32>> {
        unreachable!()
    }

    fn model_id_for(&self, _: Speed) -> String {
        "legacy-stream".into()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4_096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: sovereign_core::types::Depth::Moderate,
        }
    }
}

fn empty_capabilities() -> NodeCapabilities {
    NodeCapabilities {
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
        reported_at: 0,
        inference_availability: 1.0,
        inference_capable: false,
        loaded_models: vec![],
        embed_model: None,
        benchmark: None,
    }
}

fn member(id: NodeId, addr: SocketAddr) -> MemberRecord {
    MemberRecord {
        node_id: id,
        name: "self".into(),
        invited_by: id,
        joined_at: 0,
        last_seen: 0,
        status: NodeStatus::Online,
        capabilities: empty_capabilities(),
        addresses: vec![addr],
    }
}

fn build_state(provider: Arc<dyn InferenceProvider>) -> AppState {
    let self_id = NodeId::from_u128(0xCAFE_CAFE_CAFE_CAFE);
    let mut members = HashMap::new();
    members.insert(self_id, member(self_id, "127.0.0.1:9742".parse().unwrap()));
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "finish-reason-test".into(),
        join_key_hash: [4u8; 32],
        members,
        peers: vec![],
    };
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let state = AppState::new_with_platform_and_engine(
        self_id,
        mesh,
        mesh_store,
        app_registry,
        None,
    );
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));
    state.with_local_inference(adapter)
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, client_router(state)).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
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
    let provider: Arc<dyn InferenceProvider> = Arc::new(FixedFinishProvider {
        chunks: vec!["partial ".into(), "output ".into(), "clipped".into()],
        terminal: FinishReason::Length,
    });
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
    let reason = terminal_finish_reason(&payloads).expect(
        "stream must include a terminal chunk with a non-null finish_reason",
    );
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
    let provider: Arc<dyn InferenceProvider> = Arc::new(FixedFinishProvider {
        chunks: vec!["safe prelude ".into()],
        terminal: FinishReason::ContentFilter,
    });
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
    let provider: Arc<dyn InferenceProvider> = Arc::new(LegacyStreamProvider {
        chunks: vec!["clean ".into(), "ending".into()],
    });
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
