// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end proof of the routing decision record — Phase 0 (P1–P4)
//! of `docs/specs/SCHEDULER_QUALITY.md`.
//!
//! The unit tests inside `decision_log` / `decision_trace` pin the
//! record *shapes*. What they cannot show is the thing the whole
//! phase exists for: that a real request through the real
//! `InferenceRouter` produces a decision and an outcome that
//! **join**, carrying inputs that match what the scorer actually saw.
//! So these tests drive the production code paths —
//! `complete_stream_with_id`, `complete`, the manifest fetch, the
//! failover cascade — against a mock peer, and assert on the records
//! that fall out.
//!
//! The load-bearing assertions, in order of what they protect:
//!
//! 1. **The join closes.** Every request produces exactly one
//!    decision and one outcome sharing a `decision_id`. Without this
//!    the calibration contract (§5) has nothing to compare and Tier-1
//!    numbers are inadmissible.
//! 2. **P2 provenance is real, not defaulted.** The recorded gossip
//!    age, manifest age and load source match the endpoint the
//!    scorer was handed — a record full of `None` would look healthy
//!    and measure nothing.
//! 3. **Non-selection is recorded.** A peer excluded before scoring
//!    appears with a reason; a gated request names its gate. "The hub
//!    lost" and "the hub was never considered" must stay
//!    distinguishable in hindsight.
//! 4. **Failover is visible.** A peer that fails leaves a
//!    `FailoverAttempt` and bumps `attempt_index`, so the §5 waste
//!    metric is computable.
//! 5. **A capture round-trips.** JSONL written by the production sink
//!    loads back through `SchedulerTrace` at a 1.0 join rate.

// The topic halves live in sibling files: together they put this one
// over the §3.2 size ceiling. Explicit `#[path]` is load-bearing —
// for a `#[path]`-loaded module a child `mod` resolves against the
// CONTAINING directory, not a file-stem directory.
#[path = "scheduler_decision_records/capture_neutrality.rs"]
mod capture_neutrality;
#[path = "scheduler_decision_records/f9_local_load.rs"]
mod f9_local_load;
#[path = "scheduler_decision_records/named_path.rs"]
mod named_path;
#[path = "scheduler_decision_records/non_streaming_ranked.rs"]
mod non_streaming_ranked;
#[path = "scheduler_decision_records/streaming_ranked.rs"]
mod streaming_ranked;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::response::{sse::Event, IntoResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::ids::NodeId;
use oicp_types::{
    BenchmarkResult, CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass,
    ModelStatus, ProviderManifest, ProviderModel, ShardingPrivacy, OICP_VERSION,
};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, Speed};
use sovereign_daemon::daemon::InferenceVenue;
use sovereign_mesh::decision_log::{
    CaptureDecisionSink, DecisionSink, RoutingDecision, RoutingOutcome,
};
use sovereign_mesh::peer_inference::{InferenceRouter, VenueHost, VenueSource};

use crate::common::TestProvider;

// ── Harness ─────────────────────────────────────────────────────

pub(crate) struct StubVenueSource {
    peers: Vec<InferenceVenue>,
}

#[async_trait]
impl VenueSource for StubVenueSource {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        self.peers.clone()
    }
}

#[async_trait]
impl VenueHost for StubVenueSource {}

pub(crate) const PEER_TEXT: &str = "Answer from the peer slot.";

fn peer_manifest() -> ProviderManifest {
    ProviderManifest {
        oicp_version: OICP_VERSION.into(),
        provider: None,
        models: vec![ProviderModel {
            id: "Qwen3.5-9B.test".into(),
            base_model: None,
            quantization: None,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: Some(5.5),
            // Affinity and latency class chosen so the peer STRICTLY
            // beats the weak local model even after the cold-start
            // ramp (0.7), the gossiped load penalty and the 0.85
            // availability below. Measured from the decision record
            // itself: local scores 0.46, this peer 0.57.
            claims: vec![CapabilityClaim::new(
                CapabilityHint::general(),
                LatencyClass::Extended,
                32_768,
                4_000,
                0.95,
            )],
            fingerprint: None,
        }],
        knowledge: None,
        federation: None,
        features: vec![],
    }
}

async fn capabilities_handler() -> Json<ProviderManifest> {
    Json(peer_manifest())
}

/// Answers BOTH shapes, chosen by the request's own `stream` flag —
/// the same content negotiation a real peer daemon does. Before
/// 2026-08-06 this mock only spoke SSE, which is why no test had ever
/// driven `complete()` (the non-streaming path) against a peer, and
/// why that path's missing decision record went unnoticed.
async fn chat_completions_handler(body: Json<serde_json::Value>) -> axum::response::Response {
    let streaming = body
        .0
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !streaming {
        return Json(serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "model": "Qwen3.5-9B.test",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": PEER_TEXT },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12 }
        }))
        .into_response();
    }
    sse_completions().into_response()
}

fn sse_completions() -> impl IntoResponse {
    let delta = |s: &str| {
        serde_json::json!({
            "choices": [{ "index": 0, "delta": { "content": s }, "finish_reason": null }]
        })
        .to_string()
    };
    let (a, b) = PEER_TEXT.split_at(PEER_TEXT.len() / 2);
    let events = vec![
        Ok::<_, std::convert::Infallible>(Event::default().data(delta(a))),
        Ok(Event::default().data(delta(b))),
        Ok(Event::default().data("[DONE]")),
    ];
    Sse::new(futures::stream::iter(events)).into_response()
}

/// Serves a manifest but refuses every completion — the failover
/// scenario. `503` is deliberate: it is the congestion signal F4 says
/// the code currently conflates with failure, and the record has to
/// be able to tell them apart even though the code cannot yet.
async fn shedding_completions_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "503 Service Unavailable",
    )
}

pub(crate) async fn spawn_peer(shedding: bool) -> SocketAddr {
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities_handler))
        .route(
            "/v1/chat/completions",
            if shedding {
                post(shedding_completions_handler)
            } else {
                post(chat_completions_handler)
            },
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// A peer endpoint whose gossip signals are all populated, so P2
/// provenance has something real to record.
pub(crate) fn peer_endpoint(name: &str, addr: SocketAddr, gossip_age_secs: u64) -> InferenceVenue {
    let now = sovereign_core::time::unix_now_u64();
    InferenceVenue {
        node_id: NodeId::from_u128(0x42 << 120),
        name: name.into(),
        base_urls: vec![format!("http://{addr}/v1")],
        system_ram_gb: 64,
        benchmark: Some(BenchmarkResult {
            baseline_model_id: "baseline".into(),
            baseline_size_gb: 4.0,
            pp_tok_s: 420.0,
            tg_tok_s: 40.0,
            measured_at: now.saturating_sub(3600),
        }),
        current_in_flight: Some(3),
        inference_availability: Some(0.85),
        gossip_last_seen_unix: now.saturating_sub(gossip_age_secs),
        pinned_transport: false,
    }
}

/// An endpoint pointing nowhere — its manifest fetch fails, which is
/// the `ManifestUnavailable` exclusion.
pub(crate) fn dead_peer_endpoint(name: &str) -> InferenceVenue {
    InferenceVenue {
        node_id: NodeId::from_u128(0x99 << 120),
        name: name.into(),
        // Reserved-for-documentation address: guaranteed unroutable,
        // so the fetch fails fast on its own timeout rather than
        // depending on nothing listening on a local port.
        base_urls: vec!["http://192.0.2.1:9/v1".into()],
        system_ram_gb: 8,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }
}

/// Weak local model: cannot beat the peer's 9B on a general/Normal
/// request, so routing crosses the wire.
pub(crate) fn weak_local() -> Arc<dyn InferenceProvider> {
    Arc::new(
        TestProvider::new()
            .with_model_id("qwen2.5-3b-instruct-q4_k_m")
            .with_stream_chunks(vec!["local ".into(), "answer".into()])
            .with_complete_text("local answer"),
    )
}

pub(crate) fn mesh_request() -> CompletionRequest {
    CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(ShardingPrivacy::MeshAllowed),
        )
}

pub(crate) fn build(peers: Vec<InferenceVenue>) -> (InferenceRouter, Arc<CaptureDecisionSink>) {
    let capture = Arc::new(CaptureDecisionSink::new());
    let sink: Arc<dyn DecisionSink> = capture.clone();
    let provider = InferenceRouter::with_peer_source(
        weak_local(),
        Arc::new(StubVenueSource {
            peers: peers.clone(),
        }) as Arc<dyn VenueSource>,
        Arc::new(StubVenueSource { peers }) as Arc<dyn VenueHost>,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    )
    .with_decision_sink(sink);
    (provider, capture)
}

/// The outcome half is emitted from the stream wrapper's `Drop`,
/// which spawns onto the runtime. Yield until it lands rather than
/// sleeping a fixed amount.
pub(crate) async fn await_outcome(capture: &CaptureDecisionSink) -> RoutingOutcome {
    for _ in 0..200 {
        if let Some(o) = capture.outcomes().into_iter().next() {
            return o;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "no outcome record was emitted within 2s — the decision→outcome join is broken; \
         events seen: {:#?}",
        capture.events()
    );
}

pub(crate) fn only_decision(capture: &CaptureDecisionSink) -> RoutingDecision {
    let mut ds = capture.decisions();
    assert_eq!(
        ds.len(),
        1,
        "expected exactly one decision record, got {}: {:#?}",
        ds.len(),
        ds
    );
    ds.remove(0)
}
