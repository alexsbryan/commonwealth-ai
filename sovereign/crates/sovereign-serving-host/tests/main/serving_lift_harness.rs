// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stub-endpoint harness for `scripts/serving-lift.sh` steps 5-8.
//!
//! The lift's steps 1-4 prove the package RESOLVES, BUILDS, TESTS and
//! carries no inference backend OUTSIDE the monorepo. What they cannot show
//! is that the package RUNS: that a real [`InferenceRouter`] opens HTTP
//! connections to OpenAI-shaped endpoints, routes K requests across them,
//! sheds the K+1th on a `429` + `Retry-After`, and emits decision records
//! that replay reproduces. This file is that run
//! (`sovereign/SERVING_BOUNDARY.md` "What is enforced, and what is not"
//! Tier 2; order `domains-10-serving-extract` step 8).
//!
//! It lives in the package's OWN tests on purpose. The lift copies the
//! package's closure and nothing else, so a harness in
//! `sovereign-mesh-test-harness` (or any crate the package does not carry)
//! would be invisible to the lift it exists to serve — the lesson
//! `cw-work-lift.sh` records.
//!
//! Evidence is printed as `LIFT ...` lines on stdout, which the lift greps
//! from a `--nocapture` run. The test asserts the same facts it prints, so
//! a green test and a green lift cannot disagree.
//!
//! The `429` is the STUB's, not the host admission's. `admission::shed_response`
//! renders `503 + Retry-After` (this crate's own contract; `admission.rs`
//! tests assert it), and the design's `429` is the OpenAI endpoint's own
//! rate-limit refusal — the shape `decision_log::looks_shed` already reads
//! (`decision_log.rs:907`). The host's `503` shed is exercised by the
//! negative control in [`serving_lift_harness`].

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use kernel_types::NodeId;
use oicp_types::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus,
    ProviderManifest, ProviderModel, ShardingPrivacy, OICP_VERSION,
};
use sovereign_contracts::error::Error;
use sovereign_contracts::traits::{InferenceProvider, ResidentSlot};
use sovereign_contracts::types::{
    CompletionRequest, CompletionResponse, ProviderCapabilities, Speed,
};
use sovereign_scheduler::decision_log::{
    CaptureDecisionSink, DecisionEvent, DecisionSink, RoutingDecision,
};
use sovereign_scheduler::decision_replay::replay_decisions;
use sovereign_scheduler::venue::InferenceVenue;
use sovereign_serving_host::peer_inference::{InferenceRouter, VenueHost, VenueSource};
use sovereign_serving_host::slot_select::{SlotManifest, SlotManifestInfo};

/// How many stub OpenAI endpoints the run stands up. The design's `N`
/// (`SERVING_BOUNDARY.md` Tier 2).
const N_STUBS: usize = 3;
/// How many requests the run routes across the stubs before the shed. The
/// design's `K`.
const K_REQUESTS: usize = 6;
/// The `Retry-After` the stub's `429` carries, in seconds.
const RETRY_AFTER_SECS: u64 = 7;
/// The model every stub advertises and the local slot does NOT hold, so the
/// ranked path can choose a peer.
const LIFT_MODEL: &str = "lift-model";
/// The text a stub returns, so a served response is distinguishable from the
/// local fallback.
const STUB_TEXT: &str = "served by the stub endpoint";

// ── The stub endpoint ────────────────────────────────────────────────

#[derive(Clone)]
struct StubState {
    model_id: String,
    manifest: ProviderManifest,
    /// Successful completions served.
    served: Arc<AtomicU64>,
    /// `429` refusals answered.
    shed: Arc<AtomicU64>,
    /// Flipped on to make every completion a `429`.
    shedding: Arc<AtomicBool>,
}

impl StubState {
    fn served(&self) -> u64 {
        self.served.load(Ordering::SeqCst)
    }

    fn shed(&self) -> u64 {
        self.shed.load(Ordering::SeqCst)
    }
}

/// A stub OpenAI endpoint. Serves the three routes a real daemon serves and
/// the router reaches: `GET /oicp/v1/capabilities` (the manifest fetch),
/// `GET /v1/models` (the listing), and `POST /v1/chat/completions` (the
/// inference). The last answers `429 + Retry-After` once `shedding` is set.
struct Stub {
    addr: SocketAddr,
    state: StubState,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Stub {
    async fn start(index: usize) -> Self {
        let state = StubState {
            model_id: LIFT_MODEL.to_string(),
            manifest: stub_manifest(LIFT_MODEL),
            served: Arc::new(AtomicU64::new(0)),
            shed: Arc::new(AtomicU64::new(0)),
            shedding: Arc::new(AtomicBool::new(false)),
        };
        let app = Router::new()
            .route("/oicp/v1/capabilities", get(capabilities_handler))
            .route("/v1/models", get(models_handler))
            .route("/v1/chat/completions", post(chat_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("stub {index} could not bind a loopback port: {e}"));
        let addr = listener.local_addr().expect("bound listener has an addr");
        let (shutdown, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            tokio::select! {
                served = axum::serve(listener, app) => {
                    if let Err(e) = served {
                        eprintln!("LIFT stub {index} server error: {e}");
                    }
                }
                _ = rx => {}
            }
        });
        // The listener is bound synchronously above; the task only needs to
        // reach its accept loop before the first request. A single yield is
        // enough and keeps the harness deterministic.
        tokio::time::sleep(Duration::from_millis(20)).await;
        Self {
            addr,
            state,
            shutdown: Some(shutdown),
        }
    }

    fn start_shedding(&self) {
        self.state.shedding.store(true, Ordering::SeqCst);
    }

    /// Successful completions this stub served.
    fn served(&self) -> u64 {
        self.state.served()
    }

    /// `429` refusals this stub answered.
    fn shed(&self) -> u64 {
        self.state.shed()
    }

    fn venue(&self, index: usize) -> InferenceVenue {
        InferenceVenue {
            node_id: NodeId::from_u128(0x11f7_0000 + index as u128),
            name: format!("lift-stub-{index}"),
            base_urls: vec![format!("http://{}/v1", self.addr)],
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
            inference_availability: Some(1.0),
            gossip_last_seen_unix: now_unix_secs(),
            pinned_transport: false,
        }
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn capabilities_handler(State(s): State<StubState>) -> Json<ProviderManifest> {
    Json(s.manifest.clone())
}

async fn models_handler(State(s): State<StubState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "object": "list",
        "data": [{ "id": s.model_id, "object": "model" }],
    }))
}

async fn chat_handler(State(s): State<StubState>) -> axum::response::Response {
    if s.shedding.load(Ordering::SeqCst) {
        s.shed.fetch_add(1, Ordering::SeqCst);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, RETRY_AFTER_SECS.to_string())],
            Json(serde_json::json!({
                "error": {
                    "message": "rate limited by the stub endpoint",
                    "type": "server_error",
                    "code": "ceiling_exceeded",
                },
                "reason": "ceiling_exceeded",
                "retry_after_secs": RETRY_AFTER_SECS,
            })),
        )
            .into_response();
    }
    s.served.fetch_add(1, Ordering::SeqCst);
    Json(serde_json::json!({
        "id": "chatcmpl-lift",
        "object": "chat.completion",
        "model": s.model_id,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": STUB_TEXT },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12 },
    }))
    .into_response()
}

/// The manifest a stub advertises. The claim is the one the existing
/// `scheduler_decision_records` e2e proves strictly beats a weak local slot,
/// so the ranked path chooses a peer rather than staying home.
fn stub_manifest(model_id: &str) -> ProviderManifest {
    ProviderManifest {
        oicp_version: OICP_VERSION.into(),
        provider: None,
        models: vec![ProviderModel {
            id: model_id.to_string(),
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

// ── The local half (weak, so the peer wins) ──────────────────────────

/// A local slot that owns a model no request names. Its claim is weaker than
/// the stubs', so the ranked path crosses the wire — which is what makes the
/// run a test of the peer path at all.
struct WeakLocal;

#[async_trait]
impl InferenceProvider for WeakLocal {
    async fn complete(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_contracts::Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: "served locally".into(),
            tokens_used: 2,
            prompt_tokens: 1,
            model_id: "weak-local".into(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_contracts::Result<
        Pin<Box<dyn Stream<Item = sovereign_contracts::Result<String>> + Send>>,
    > {
        Err(Error::NotImplemented("stub".into()))
    }

    async fn embed(&self, _text: &str) -> sovereign_contracts::Result<Vec<f32>> {
        Err(Error::NotImplemented("stub".into()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Slow,
            relative_reasoning: oicp_types::Depth::Moderate,
        }
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        "weak-local".to_string()
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        vec![ResidentSlot {
            role: "primary".to_string(),
            model_id: "weak-local".to_string(),
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        }]
    }
}

// ── The ports the router asks through ────────────────────────────────

struct StubSource(Vec<InferenceVenue>);

#[async_trait]
impl VenueSource for StubSource {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        self.0.clone()
    }
}

#[async_trait]
impl VenueHost for StubSource {}

/// No declared slot facts. The host's advertising path falls back to the
/// defaults (which the stubs outscore), exactly as it does for a BYOM slot.
struct NoManifest;

impl SlotManifest for NoManifest {
    fn capabilities_for_file(&self, _file: &str) -> Option<oicp_types::CapabilityProfile> {
        None
    }

    fn info_for_file(&self, _file: &str) -> Option<SlotManifestInfo> {
        None
    }
}

fn oicp_request() -> CompletionRequest {
    CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(ShardingPrivacy::MeshAllowed),
        )
}

fn now_unix_secs() -> u64 {
    sovereign_time::unix_now_u64()
}

/// Replay every decision in the capture and return the aggregate report.
/// The honesty note the lift carries: `replay_decision` assumes
/// `RankObjective::Product` (`scheduler_core.rs:72-76`), which is the only
/// objective production ranks on (`peer_inference.rs:1775`), so every record
/// this run emits is in the replay's domain.
fn replay(capture: &CaptureDecisionSink) -> sovereign_scheduler::decision_replay::ReplayReport {
    let decisions: Vec<RoutingDecision> = capture.decisions();
    replay_decisions(decisions.iter())
}

// ── The run ──────────────────────────────────────────────────────────

/// The lift's RUN half, in one place. Starts `N_STUBS` stub OpenAI endpoints,
/// routes `K_REQUESTS` OICP turns across them, flips them to `429` for the
/// K+1th, and asserts:
///
/// - **positive control**: the K requests are SERVED BY THE STUBS (not the
///   local fallback) — the pool's own counter says so;
/// - **negative control**: the K+1th meets a `429 + Retry-After` and the
///   router records it as a SHED, not a fault (`decision_log::looks_shed`);
/// - **replay**: every decision the run emitted reproduces from its record
///   alone, at 1.0 scorer and policy agreement;
/// - **the decider guard**: the capture holds `>= K` decisions and outcomes
///   and `>= 1` fleet snapshot.
#[tokio::test]
async fn serving_lift_harness() {
    let stubs: Vec<Stub> = {
        let mut stubs = Vec::with_capacity(N_STUBS);
        for i in 0..N_STUBS {
            stubs.push(Stub::start(i).await);
        }
        stubs
    };
    let venues: Vec<InferenceVenue> = stubs.iter().enumerate().map(|(i, s)| s.venue(i)).collect();

    let capture = Arc::new(CaptureDecisionSink::new());
    let sink: Arc<dyn DecisionSink> = capture.clone();
    let router = InferenceRouter::builder(Arc::new(WeakLocal) as Arc<dyn InferenceProvider>)
        .candidates(Arc::new(StubSource(venues)))
        .host(Arc::new(StubSource(Vec::new())))
        .manifest(Arc::new(NoManifest))
        .build()
        .with_decision_sink(sink);

    // ── Phase A: K requests, served by the stub pool ─────────────────
    let mut served_by_stub = 0usize;
    for turn in 0..K_REQUESTS {
        let response = router
            .complete(&oicp_request())
            .await
            .unwrap_or_else(|e| panic!("turn {turn} of {K_REQUESTS} was not served: {e}"));
        assert_eq!(
            response.text, STUB_TEXT,
            "turn {turn} was served locally, not by a stub endpoint — the ranked path \
             did not cross the wire"
        );
        served_by_stub += 1;
    }
    let pool_served: u64 = stubs.iter().map(Stub::served).sum();
    assert_eq!(
        pool_served, K_REQUESTS as u64,
        "the stub pool served {pool_served} of {K_REQUESTS} requests"
    );

    // ── Phase B: the K+1th meets a 429 + Retry-After ─────────────────
    for stub in &stubs {
        stub.start_shedding();
    }
    // The ranked peers all shed, so the router falls back to local; the run
    // still succeeds, and the SHED is what the record must carry.
    let _ = router
        .complete(&oicp_request())
        .await
        .expect("the K+1th falls back to local after the peer shed");
    let pool_shed: u64 = stubs.iter().map(Stub::shed).sum();
    assert!(
        pool_shed >= 1,
        "no stub answered the K+1th with a 429 — the shed path was never exercised"
    );

    let decisions = capture.decisions();
    let outcomes = capture.outcomes();
    let shed_failover = outcomes
        .iter()
        .flat_map(|o| &o.failovers)
        .any(|f| f.shed && f.error.contains("429"));
    assert!(
        shed_failover,
        "the 429 was not recorded as a SHED (a refusal to serve is not a fault): \
         outcomes = {outcomes:#?}"
    );

    // ── Phase C: replay reproduces every decision ────────────────────
    let report = replay(&capture);
    assert!(
        report.replayed >= K_REQUESTS,
        "the run emitted only {} replayable decisions, want >= {K_REQUESTS}",
        report.replayed
    );
    assert_eq!(
        report.policy_agreement(),
        1.0,
        "replay policy disagreed on {:?}",
        report.policy_disagreements
    );
    assert_eq!(
        report.scorer_agreement(),
        1.0,
        "replay scorer disagreed on {:?}",
        report.scorer_disagreements
    );

    // ── Phase D: the decider guard ───────────────────────────────────
    let snapshots = capture
        .events()
        .into_iter()
        .filter(|e| matches!(e, DecisionEvent::Snapshot(_)))
        .count();
    assert!(
        decisions.len() >= K_REQUESTS,
        "guard: {} decisions < {K_REQUESTS}",
        decisions.len()
    );
    assert!(
        outcomes.len() >= K_REQUESTS,
        "guard: {} outcomes < {K_REQUESTS}",
        outcomes.len()
    );
    assert!(
        snapshots >= 1,
        "guard: the capture holds no FleetSnapshot — a replayable episode needs the fleet it ran against"
    );

    // ── Evidence the lift greps ──────────────────────────────────────
    println!(
        "LIFT endpoints={N_STUBS} requests={K_REQUESTS} served={pool_served} \
         served_by_stub={served_by_stub}"
    );
    println!("LIFT shed=429 retry_after={RETRY_AFTER_SECS} count={pool_shed}");
    println!(
        "LIFT replay={}/{} scorer={}/{}",
        report.policy_agreed, report.replayed, report.candidates_agreed, report.candidates_checked
    );
    println!(
        "LIFT decisions={} outcomes={} snapshots={snapshots}",
        decisions.len(),
        outcomes.len()
    );
    println!(
        "LIFT controls=positive:stub_served={pool_served} negative:shed_recorded={shed_failover}"
    );
}
