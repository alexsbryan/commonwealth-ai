// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test helpers for sovereign-mesh integration tests.
//!
//! Used by tests under `tests/*.rs` via `mod common;`. Each helper
//! is intentionally small and parameterizable. Premature flexibility
//! is more expensive than a few duplicated lines per ARCH §10.3, so
//! the bar to add a knob here is "two callers need it" not "one
//! caller might".
//!
//! Rust's integration-test layout treats `tests/common/mod.rs` as a
//! shared module — NOT a separate test binary the way `tests/common.rs`
//! would be. Each consumer adds `mod common;` at the top of their
//! test file.

#![allow(dead_code)]
// Every test binary uses a different subset of these helpers; the
// `dead_code` lint would otherwise fire per-binary on the unused ones.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use futures::Stream;

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use sovereign_core::error::{Error, Result as SovResult};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, ProviderCapabilities, Speed, StreamFrame,
};

// ── Capabilities + member helpers ───────────────────────────────

/// A `NodeCapabilities` with every field zeroed / empty. Useful for
/// constructing test `MemberRecord`s where the hardware profile
/// doesn't matter.
pub fn empty_capabilities() -> NodeCapabilities {
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
        current_in_flight: None,
    }
}

/// Build a `MemberRecord` with a specified `last_seen`. Use when the
/// test cares about the timestamp (e.g. gossip-decay scenarios).
pub fn member_with_last_seen(
    id: NodeId,
    name: &str,
    last_seen: u64,
    addr: SocketAddr,
) -> MemberRecord {
    MemberRecord {
        node_pubkey: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen,
        status: NodeStatus::Online,
        capabilities: empty_capabilities(),
        addresses: vec![addr],
    }
}

/// Build a `MemberRecord` with `last_seen = 0`. The common case in
/// tests that don't exercise decay.
pub fn member(id: NodeId, name: &str, addr: SocketAddr) -> MemberRecord {
    member_with_last_seen(id, name, 0, addr)
}

/// Build a single-member `Mesh` rooted at `self_id`. The mesh_id is
/// 1 and the join_key_hash is `[0x77; 32]` — neither matters for
/// tests that don't exercise the gossip auth boundary; for those
/// tests, construct the mesh inline with the right values.
pub fn solo_mesh(self_id: NodeId, name: &str) -> Mesh {
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    Mesh {
        id: MeshId::from_u128(1),
        name: name.into(),
        join_key_hash: [0x77u8; 32],
        members,
        peers: vec![],
    }
}

/// Hex-encode a `NodeId` for the `X-Node-Id` header. 32 hex chars,
/// lowercase — matches `commonwealth_api::headers::parse_x_node_id`.
pub fn id_to_hex(id: &NodeId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

// ── Router spawning ─────────────────────────────────────────────

/// Bind `router` on `127.0.0.1:0` and return the bound address. The
/// listener is wired with `into_make_service_with_connect_info::<SocketAddr>()`
/// so the loopback guard middleware (which fail-closes on absent
/// ConnectInfo) sees the production listener shape.
pub async fn spawn_router(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    // 20ms is enough headroom on every CI box we use; the tokio
    // accept-loop is ready well before reqwest's first connect.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

// ── Configurable InferenceProvider stub ─────────────────────────

/// A builder-style `InferenceProvider` used across integration tests.
///
/// Defaults to "every method returns `NotImplemented`". Tests opt
/// into specific behaviors via `with_*` builder methods. The intent
/// is to make the per-test code expressive about what the stub
/// supports — a test that never exercises `embed` doesn't have to
/// configure it, and a future regression that starts calling it
/// surfaces as a `NotImplemented` error rather than silent success.
///
/// Replaces the per-file `LocalStub` / `StubProvider` / `EmbedStub`
/// / `NoopProvider` / `ManifestProvider` / `FixedFinishProvider` /
/// `LegacyStreamProvider` copies that accumulated as the test suite
/// grew. ARCH §10.3's "four or more" threshold for trait extraction
/// is exceeded; this is that extraction.
pub struct TestProvider {
    model_id: String,
    code_model_id: Option<String>,
    complete_text: Option<String>,
    stream_chunks: Option<Vec<String>>,
    embed_fn: Option<Arc<dyn Fn(&str) -> Vec<f32> + Send + Sync>>,
    /// When set, `complete_stream_with_finish` returns exactly these
    /// frames. Use to test finish_reason wire fidelity (Length,
    /// ContentFilter, etc.). When None, the trait's default impl
    /// wraps `complete_stream` and appends a synthetic Stop.
    typed_frames: Option<Vec<StreamFrame>>,
    /// Capabilities reported to manifest synthesis. The test rarely
    /// inspects this beyond a sanity check; defaults are conservative.
    capabilities: ProviderCapabilities,
}

impl TestProvider {
    pub fn new() -> Self {
        Self {
            model_id: "test-provider".into(),
            code_model_id: None,
            complete_text: None,
            stream_chunks: None,
            embed_fn: None,
            typed_frames: None,
            capabilities: ProviderCapabilities {
                max_context_tokens: 4_096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: sovereign_core::types::Depth::Moderate,
            },
        }
    }

    pub fn with_model_id(mut self, id: impl Into<String>) -> Self {
        self.model_id = id.into();
        self
    }

    pub fn with_code_model_id(mut self, id: impl Into<String>) -> Self {
        self.code_model_id = Some(id.into());
        self
    }

    /// `complete()` returns a `CompletionResponse` carrying this text.
    pub fn with_complete_text(mut self, text: impl Into<String>) -> Self {
        self.complete_text = Some(text.into());
        self
    }

    /// `complete_stream()` (legacy `Result<String>` surface) yields
    /// these chunks in order. The default-impl
    /// `complete_stream_with_finish` then wraps them with a synthetic
    /// terminal `Stop`. To override the terminal frame, use
    /// [`Self::with_typed_frames`].
    pub fn with_stream_chunks(mut self, chunks: Vec<String>) -> Self {
        self.stream_chunks = Some(chunks);
        self
    }

    /// `embed(input)` runs this closure on the input and returns the
    /// resulting vector. Tests that want a marker-encoded vector
    /// (e.g. `|input| vec![input.len() as f32; 8]`) pass a closure;
    /// tests that just want a zero vector pass `|_| vec![0.0; N]`.
    pub fn with_embed_marker(
        mut self,
        f: impl Fn(&str) -> Vec<f32> + Send + Sync + 'static,
    ) -> Self {
        self.embed_fn = Some(Arc::new(f));
        self
    }

    /// `complete_stream_with_finish()` yields these typed frames.
    /// Use the `StreamFrame::Finish { reason, .. }` variant to pin
    /// non-Stop finish reasons.
    pub fn with_typed_frames(mut self, frames: Vec<StreamFrame>) -> Self {
        self.typed_frames = Some(frames);
        self
    }
}

impl Default for TestProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InferenceProvider for TestProvider {
    async fn complete(&self, _req: &CompletionRequest) -> SovResult<CompletionResponse> {
        match self.complete_text.as_ref() {
            Some(t) => Ok(CompletionResponse {
                text: t.clone(),
                tokens_used: 1,
                prompt_tokens: 1,
                model_id: self.model_id.clone(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            }),
            None => Err(Error::NotImplemented(
                "TestProvider::complete not configured — \
                 call .with_complete_text(...) on the builder"
                    .into(),
            )),
        }
    }

    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        match self.stream_chunks.as_ref() {
            Some(chunks) => {
                let items: Vec<SovResult<String>> = chunks.iter().cloned().map(Ok).collect();
                Ok(Box::pin(futures::stream::iter(items)))
            }
            None => Err(Error::NotImplemented(
                "TestProvider::complete_stream not configured — \
                 call .with_stream_chunks(...) on the builder"
                    .into(),
            )),
        }
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        if let Some(frames) = self.typed_frames.as_ref() {
            return Ok(Box::pin(futures::stream::iter(frames.clone())));
        }
        // Reproduce the trait's default impl inline — we can't
        // dispatch to it without infinite recursion. Wraps
        // `complete_stream` with `Token(text)` frames and appends
        // a synthetic terminal `Stop` (unless the body already
        // emitted an `Error` terminator). Matches the documented
        // behaviour of `InferenceProvider::complete_stream_with_finish`'s
        // default impl in `sovereign-core::traits`.
        use futures::StreamExt;
        use std::sync::atomic::{AtomicBool, Ordering};

        let inner = self.complete_stream(request).await?;
        let terminal_emitted = Arc::new(AtomicBool::new(false));
        let body_flag = Arc::clone(&terminal_emitted);
        let mapped = inner.flat_map(move |item| {
            let frames: Vec<StreamFrame> = match item {
                Ok(text) => vec![StreamFrame::Token(text)],
                Err(e) => {
                    body_flag.store(true, Ordering::Relaxed);
                    vec![StreamFrame::Finish {
                        reason: sovereign_core::types::FinishReason::Error(format!("{e}")),
                        usage: None,
                    }]
                }
            };
            futures::stream::iter(frames)
        });
        let tail_flag = terminal_emitted;
        let tail = futures::stream::once(async move {
            if tail_flag.load(Ordering::Relaxed) {
                None
            } else {
                Some(StreamFrame::Finish {
                    reason: sovereign_core::types::FinishReason::Stop,
                    usage: None,
                })
            }
        })
        .filter_map(|f| async move { f });
        Ok(Box::pin(mapped.chain(tail)))
    }

    async fn embed(&self, input: &str) -> SovResult<Vec<f32>> {
        match self.embed_fn.as_ref() {
            Some(f) => Ok(f(input)),
            None => Err(Error::NotImplemented(
                "TestProvider::embed not configured — \
                 call .with_embed_marker(...) on the builder"
                    .into(),
            )),
        }
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        self.model_id.clone()
    }

    fn code_model_id(&self) -> Option<String> {
        self.code_model_id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }
}
