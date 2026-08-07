// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end proof of the routing decision record — Phase 0 (P1–P4)
//! of `docs/specs/SCHEDULER_QUALITY.md`.
//!
//! The unit tests inside `decision_log` / `decision_trace` pin the
//! record *shapes*. What they cannot show is the thing the whole
//! phase exists for: that a real request through the real
//! `MeshInferenceProvider` produces a decision and an outcome that
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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::response::{sse::Event, IntoResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::ids::NodeId;
use futures::StreamExt;
use sovereign_core::oicp::{
    BenchmarkResult, CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass,
    ModelStatus, ProviderManifest, ProviderModel, ShardingPrivacy, OICP_VERSION,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_mesh::daemon::PeerInferenceEndpoint;
use sovereign_mesh::decision_log::{
    CandidateKind, CaptureDecisionSink, DecisionEvent, DecisionPath, DecisionSink, ExclusionReason,
    LoadSource, RoutingDecision, RoutingOutcome, ServedBy, TracingDecisionSink, Verdict,
};
use sovereign_mesh::decision_trace::SchedulerTrace;
use sovereign_mesh::peer_inference::{MeshInferenceProvider, PeerEndpointSource};

mod common;
use common::TestProvider;

// ── Harness ─────────────────────────────────────────────────────

struct StubPeerSource {
    peers: Vec<PeerInferenceEndpoint>,
}

#[async_trait]
impl PeerEndpointSource for StubPeerSource {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        self.peers.clone()
    }
}

const PEER_TEXT: &str = "Answer from the peer slot.";

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

async fn spawn_peer(shedding: bool) -> SocketAddr {
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
fn peer_endpoint(name: &str, addr: SocketAddr, gossip_age_secs: u64) -> PeerInferenceEndpoint {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    PeerInferenceEndpoint {
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
        transport: None,
    }
}

/// An endpoint pointing nowhere — its manifest fetch fails, which is
/// the `ManifestUnavailable` exclusion.
fn dead_peer_endpoint(name: &str) -> PeerInferenceEndpoint {
    PeerInferenceEndpoint {
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
        transport: None,
    }
}

/// Weak local model: cannot beat the peer's 9B on a general/Normal
/// request, so routing crosses the wire.
fn weak_local() -> Arc<dyn InferenceProvider> {
    Arc::new(
        TestProvider::new()
            .with_model_id("qwen2.5-3b-instruct-q4_k_m")
            .with_stream_chunks(vec!["local ".into(), "answer".into()])
            .with_complete_text("local answer"),
    )
}

fn mesh_request() -> CompletionRequest {
    CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(ShardingPrivacy::MeshAllowed),
        )
}

fn build(
    peers: Vec<PeerInferenceEndpoint>,
) -> (MeshInferenceProvider, Arc<CaptureDecisionSink>) {
    let capture = Arc::new(CaptureDecisionSink::new());
    let sink: Arc<dyn DecisionSink> = capture.clone();
    let provider = MeshInferenceProvider::with_peer_source(
        weak_local(),
        Arc::new(StubPeerSource { peers }) as Arc<dyn PeerEndpointSource>,
    )
    .with_decision_sink(sink);
    (provider, capture)
}

/// The outcome half is emitted from the stream wrapper's `Drop`,
/// which spawns onto the runtime. Yield until it lands rather than
/// sleeping a fixed amount.
async fn await_outcome(capture: &CaptureDecisionSink) -> RoutingOutcome {
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

fn only_decision(capture: &CaptureDecisionSink) -> RoutingDecision {
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

// ── 1. The join closes, on the streaming peer path ──────────────

#[tokio::test]
async fn peer_routed_stream_emits_a_joined_decision_and_outcome() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 12)]);

    let (mut stream, attribution) = provider
        .complete_stream_with_id(&mesh_request())
        .await
        .expect("peer route should succeed");
    assert!(attribution.contains("@ peer hub"), "got {attribution:?}");

    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        body.push_str(&chunk.unwrap());
    }
    assert_eq!(body, PEER_TEXT);
    drop(stream);

    let decision = only_decision(&capture);
    assert_eq!(decision.path, DecisionPath::RankedOicp);
    match &decision.verdict {
        Verdict::Peers { ranked } => assert_eq!(ranked, &vec!["hub".to_string()]),
        other => panic!("expected a peer verdict, got {other:?}"),
    }

    // Both candidates are recorded — local competed and lost. A
    // record that only listed the winner could not answer "was that
    // right in hindsight".
    let names: Vec<&str> = decision.candidates.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"local"), "local must be recorded: {names:?}");
    assert!(names.contains(&"hub"), "peer must be recorded: {names:?}");

    let hub = decision.candidates.iter().find(|c| c.name == "hub").unwrap();
    let local = decision
        .candidates
        .iter()
        .find(|c| c.name == "local")
        .unwrap();
    assert!(hub.selected && hub.rank == Some(0));
    assert!(!local.selected && local.rank.is_none());
    assert_eq!(hub.kind, CandidateKind::Peer);
    assert_eq!(local.kind, CandidateKind::Local);
    // The winner's recorded score must be the score it won on.
    assert!(
        hub.score.final_score > local.score.final_score,
        "recorded scores must explain the verdict: hub {} vs local {}",
        hub.score.final_score,
        local.score.final_score
    );

    let outcome = await_outcome(&capture).await;
    assert_eq!(
        outcome.decision_id, decision.decision_id,
        "the outcome must join to its decision"
    );
    assert_eq!(outcome.attempt_index, 0);
    assert!(outcome.failovers.is_empty());
    assert!(!outcome.shed);
    match &outcome.served_by {
        ServedBy::Peer { name, model_id, .. } => {
            assert_eq!(name, "hub");
            assert!(model_id.contains("9B"), "got {model_id:?}");
        }
        other => panic!("expected peer service, got {other:?}"),
    }
    // Timings come from the same wrapper that feeds the throughput
    // EWMAs, so a populated outcome is evidence the two agree.
    assert!(outcome.ttft_ms.is_some_and(|v| v >= 0.0));
    assert!(outcome.total_ms.is_some_and(|v| v > 0.0));
    assert_eq!(outcome.output_tokens, Some(2));
}

// ── 2. P2 provenance reflects what the scorer was handed ────────

#[tokio::test]
async fn peer_candidate_records_the_provenance_of_every_scorer_input() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 17)]);

    let (stream, _) = provider
        .complete_stream_with_id(&mesh_request())
        .await
        .unwrap();
    drop(stream);

    let decision = only_decision(&capture);
    let hub = decision.candidates.iter().find(|c| c.name == "hub").unwrap();
    let inputs = &hub.inputs;

    // The gossiped count overrode this node's self-observed zero —
    // and the record says so, which is the distinction F1 turns on.
    assert_eq!(inputs.in_flight_source, LoadSource::Gossip);
    assert_eq!(inputs.in_flight, 3);
    assert_eq!(inputs.gossiped_in_flight, Some(3));
    assert_eq!(inputs.self_observed_in_flight, Some(0));
    assert_eq!(inputs.availability, Some(0.85));

    // Staleness: derived from the endpoint's `gossip_last_seen_unix`,
    // which the test set 17s in the past. Allow slack for clock
    // granularity but pin the magnitude — a defaulted `0` or a `None`
    // here would mean the provenance is decorative.
    let age = inputs
        .gossip_age_secs
        .expect("gossip age must be stamped when last_seen is known");
    assert!(
        (15..=25).contains(&age),
        "gossip age should track last_seen (~17s), got {age}"
    );

    // The manifest was fetched during this decision, not read from
    // the 60s cache.
    assert_eq!(inputs.manifest_from_cache, Some(false));
    assert_eq!(inputs.manifest_age_secs, Some(0));
    assert!(inputs.rtt_ms.is_some());

    // Benchmark and its age — the throughput-estimate path's input.
    assert_eq!(inputs.bench_tg_tok_s, Some(40.0));
    assert_eq!(inputs.bench_pp_tok_s, Some(420.0));
    let bench_age = inputs.bench_age_secs.expect("benchmark age must be stamped");
    assert!(
        (3500..=3700).contains(&bench_age),
        "benchmark age should track measured_at (~3600s), got {bench_age}"
    );

    // Local's asymmetry is stated, not implied: no staleness at all.
    let local = decision
        .candidates
        .iter()
        .find(|c| c.name == "local")
        .unwrap();
    assert_eq!(local.inputs.in_flight_source, LoadSource::Local);
    assert_eq!(local.inputs.gossip_age_secs, None);
}

/// The second request inside the manifest TTL reads from cache — and
/// the record must say so, because a cached manifest is a second,
/// independent staleness channel alongside gossip lag.
#[tokio::test]
async fn second_decision_records_the_manifest_as_cached() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 5)]);

    for _ in 0..2 {
        let (stream, _) = provider
            .complete_stream_with_id(&mesh_request())
            .await
            .unwrap();
        drop(stream);
    }

    let decisions = capture.decisions();
    assert_eq!(decisions.len(), 2);
    let first = decisions[0]
        .candidates
        .iter()
        .find(|c| c.name == "hub")
        .unwrap();
    let second = decisions[1]
        .candidates
        .iter()
        .find(|c| c.name == "hub")
        .unwrap();
    assert_eq!(first.inputs.manifest_from_cache, Some(false));
    assert_eq!(second.inputs.manifest_from_cache, Some(true));
}

// ── 3. Non-selection is recorded ────────────────────────────────

#[tokio::test]
async fn unreachable_peer_is_recorded_as_excluded_with_a_reason() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![
        peer_endpoint("hub", addr, 8),
        dead_peer_endpoint("ghost"),
    ]);

    let (stream, _) = provider
        .complete_stream_with_id(&mesh_request())
        .await
        .unwrap();
    drop(stream);

    let decision = only_decision(&capture);
    assert_eq!(
        decision.excluded.len(),
        1,
        "the unreachable peer must be recorded, not silently dropped: {:#?}",
        decision.excluded
    );
    assert_eq!(decision.excluded[0].name, "ghost");
    assert_eq!(
        decision.excluded[0].reason,
        ExclusionReason::ManifestUnavailable
    );
    // And it must NOT appear as a scored candidate.
    assert!(decision.candidates.iter().all(|c| c.name != "ghost"));
}

#[tokio::test]
async fn a_gated_request_names_its_gate_and_scores_nothing() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 8)]);

    // `LocalOnly` is the privacy contract: this must never reach a
    // peer, and the record must say why it did not.
    let request = CompletionRequest::new("private").with_speed(Speed::Slow).with_oicp(
        InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal)
            .with_sharding(ShardingPrivacy::LocalOnly),
    );
    let (stream, _) = provider.complete_stream_with_id(&request).await.unwrap();
    drop(stream);

    let decision = only_decision(&capture);
    match &decision.verdict {
        Verdict::Gated { gate } => assert_eq!(gate, "not_offload_eligible"),
        other => panic!("expected a gated verdict, got {other:?}"),
    }
    assert!(decision.candidates.is_empty());
    assert!(decision.excluded.is_empty());
    assert_eq!(decision.request.sharding, "LocalOnly");

    // The gated decision still gets an outcome — served locally.
    let outcome = await_outcome(&capture).await;
    assert_eq!(outcome.decision_id, decision.decision_id);
    assert!(matches!(outcome.served_by, ServedBy::LocalFallback { .. }));
}

// ── 4. Failover is visible ──────────────────────────────────────

#[tokio::test]
async fn a_shedding_peer_leaves_a_failover_attempt_on_the_outcome() {
    let addr = spawn_peer(true).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 9)]);

    let (mut stream, attribution) = provider
        .complete_stream_with_id(&mesh_request())
        .await
        .expect("cascade should fall back to local");
    assert!(
        !attribution.contains("@ peer"),
        "the shedding peer must not be attributed; got {attribution:?}"
    );
    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        body.push_str(&chunk.unwrap());
    }
    drop(stream);
    assert_eq!(body, "local answer");

    // The decision still chose the peer — the scorer was right about
    // the ranking and wrong about the peer's capacity. Keeping those
    // separable is the point of recording both halves.
    let decision = only_decision(&capture);
    assert!(matches!(decision.verdict, Verdict::Peers { .. }));

    let outcome = await_outcome(&capture).await;
    assert_eq!(outcome.decision_id, decision.decision_id);
    assert_eq!(
        outcome.attempt_index, 1,
        "serving from step 1 means one failover happened"
    );
    assert!(matches!(outcome.served_by, ServedBy::LocalFallback { .. }));
    assert_eq!(outcome.failovers.len(), 1);
    assert_eq!(outcome.failovers[0].peer, "hub");
    // F4: congestion and failure are one channel in the code. The
    // record classifies them apart so the Phase 2 fix has a baseline.
    assert!(
        outcome.failovers[0].shed,
        "a 503 must be classified as a shed, not a transport failure: {:?}",
        outcome.failovers[0]
    );
}

// ── 5. The non-streaming path closes its join too ───────────────

#[tokio::test]
async fn non_streaming_complete_closes_the_join() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 6)]);

    // The mock peer only speaks SSE, so `complete()` falls through to
    // local. Either way the join must close — that is the assertion.
    let _ = provider.complete(&mesh_request()).await;

    let decision = only_decision(&capture);
    let outcome = await_outcome(&capture).await;
    assert_eq!(outcome.decision_id, decision.decision_id);
    // Non-streaming has no first token to time; claiming one would be
    // a fabrication, so the field is `None` by construction.
    assert_eq!(outcome.ttft_ms, None);
    assert!(outcome.total_ms.is_some());
}

// ── 6. A capture round-trips into a replayable fixture ──────────

#[tokio::test]
async fn jsonl_capture_loads_back_as_a_replayable_trace() {
    let addr = spawn_peer(false).await;
    let dir = std::env::temp_dir().join(format!("sched-trace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("decisions.jsonl");

    let sink: Arc<dyn DecisionSink> = Arc::new(TracingDecisionSink::to_path(&path).unwrap());
    let provider = MeshInferenceProvider::with_peer_source(
        weak_local(),
        Arc::new(StubPeerSource {
            peers: vec![peer_endpoint("hub", addr, 14)],
        }) as Arc<dyn PeerEndpointSource>,
    )
    .with_decision_sink(sink);

    for _ in 0..3 {
        let (mut stream, _) = provider
            .complete_stream_with_id(&mesh_request())
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        drop(stream);
    }
    // Let the Drop-spawned outcome tasks land before reading.
    for _ in 0..200 {
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        if body.lines().count() >= 7 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let trace = SchedulerTrace::from_jsonl_path(&path).expect("capture must load");
    assert_eq!(trace.episodes.len(), 3);
    assert!(
        trace.orphan_outcomes.is_empty(),
        "orphans mean the join broke: {:#?}",
        trace.orphan_outcomes
    );
    assert_eq!(
        trace.join_rate(),
        1.0,
        "a trace below a full join rate is not admissible calibration evidence"
    );
    assert_eq!(trace.scored_episodes().count(), 3);
    for ep in &trace.episodes {
        assert_eq!(ep.chosen(), Some("hub"));
        assert!(ep.served_first_choice());
    }

    // P3: the fleet snapshot rides in the same stream, so the capture
    // is self-contained — no second collection step to forget.
    assert!(
        !trace.snapshots.is_empty(),
        "the capture must carry a fleet snapshot"
    );
    let snap = trace.snapshot_for(&trace.episodes[0]).unwrap();
    assert_eq!(snap.peers.len(), 1);
    assert_eq!(snap.peers[0].name, "hub");
    let age = snap.peers[0].gossip_age_secs.unwrap();
    assert!((12..=22).contains(&age), "snapshot gossip age was {age}");
    // The local side is described too — a fleet snapshot that only
    // listed peers would leave the sim without the node that decides.
    assert!(
        !snap.local.advertised_models.is_empty(),
        "the snapshot must describe the local node's advertised models"
    );

    // And the whole fixture survives the JSON round-trip the sim reads.
    let json = trace.to_json().unwrap();
    assert_eq!(SchedulerTrace::from_json(&json).unwrap(), trace);

    let _ = std::fs::remove_dir_all(&dir);
}

// ── 7. Instrumentation must not change routing ──────────────────

/// Phase 0's whole premise is that it changes no decision (§6). Two
/// providers differing only in their sink must route identically.
#[tokio::test]
async fn the_sink_does_not_change_the_routing_decision() {
    let addr = spawn_peer(false).await;

    let (with_capture, _) = build(vec![peer_endpoint("hub", addr, 11)]);
    let silent = MeshInferenceProvider::with_peer_source(
        weak_local(),
        Arc::new(StubPeerSource {
            peers: vec![peer_endpoint("hub", addr, 11)],
        }) as Arc<dyn PeerEndpointSource>,
    )
    .with_decision_sink(Arc::new(sovereign_mesh::decision_log::NullDecisionSink));

    let (a_stream, a_attr) = with_capture
        .complete_stream_with_id(&mesh_request())
        .await
        .unwrap();
    drop(a_stream);
    let (b_stream, b_attr) = silent
        .complete_stream_with_id(&mesh_request())
        .await
        .unwrap();
    drop(b_stream);

    assert_eq!(a_attr, b_attr);
}

// ── 8. Snapshot cadence ─────────────────────────────────────────

/// The snapshot is rate-limited so a busy node pays for it once a
/// minute, not once a request. Three back-to-back requests must
/// produce exactly one.
#[tokio::test]
async fn fleet_snapshots_are_rate_limited_not_per_request() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 7)]);

    for _ in 0..3 {
        let (stream, _) = provider
            .complete_stream_with_id(&mesh_request())
            .await
            .unwrap();
        drop(stream);
    }

    let snapshots = capture
        .events()
        .into_iter()
        .filter(|e| matches!(e, DecisionEvent::Snapshot(_)))
        .count();
    assert_eq!(
        snapshots, 1,
        "expected one snapshot across three requests, got {snapshots}"
    );
    assert_eq!(capture.decisions().len(), 3);
}

// ── 9. A soft named target that resolves to nobody falls THROUGH ─
//
// The household case this exists for: a laptop configured to send its
// primary turn into a shared 122B, on a mesh that also has a 35B hub.
// While the shared cluster is forming (or its host is down) the old
// code dropped that laptop to its OWN 4B and left the hub idle — a
// pure loss, since no latency was bought and no privacy honoured. The
// named target is soft, i.e. a preference, so the correct degradation
// is the ranked mesh, with local as the LAST rung rather than the
// second one.
//
// A hard (`model_id`-named) target is unaffected and must stay so:
// an explicit name still fails loudly rather than being silently
// substituted. That is asserted in `hard_named_*` below.

/// The one decision on `path`, and a readable panic when the
/// fallthrough produced the wrong number of them.
fn only_decision_on(capture: &CaptureDecisionSink, path: DecisionPath) -> RoutingDecision {
    let mut ds: Vec<RoutingDecision> = capture
        .decisions()
        .into_iter()
        .filter(|d| d.path == path)
        .collect();
    assert_eq!(
        ds.len(),
        1,
        "expected exactly one {path:?} decision, got {}: {:#?}",
        ds.len(),
        capture.decisions()
    );
    ds.remove(0)
}

const SHARED_PRIMARY: &str = "glm-5.2-distributed";

#[tokio::test]
async fn forming_shared_model_falls_through_to_the_mesh_not_to_the_local_model() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 12)]);
    // Configured to prefer a shared model that nobody in this mesh
    // advertises — the cluster is forming, or its host is down.
    provider.set_shared_model_id(Some(SHARED_PRIMARY.into()));

    let (mut stream, attribution) = provider
        .complete_stream_with_id(&mesh_request())
        .await
        .expect("an unavailable soft primary must not fail the request");

    // THE assertion: the free hub served, not this node's own model.
    assert!(
        attribution.contains("@ peer hub"),
        "a forming shared model must degrade to the mesh, not to local; got {attribution:?}"
    );
    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        body.push_str(&chunk.unwrap());
    }
    assert_eq!(body, PEER_TEXT);
    drop(stream);

    // Two records, one story, joined by request: the named target
    // resolved to nobody, and THEN the scorer ran. Neither alone can
    // answer "why did my 122B request get answered by the 35B?".
    let named = only_decision_on(&capture, DecisionPath::NamedModel);
    match &named.verdict {
        Verdict::NamedUnknown { model_id } => assert_eq!(model_id, SHARED_PRIMARY),
        other => panic!("expected the named target to resolve to nobody, got {other:?}"),
    }
    assert!(
        named.candidates.is_empty(),
        "name resolution scores nothing — inventing candidates would pollute the scoreboard"
    );

    let fell_through = only_decision_on(&capture, DecisionPath::NamedFallthrough);
    match &fell_through.verdict {
        Verdict::Peers { ranked } => assert_eq!(ranked, &vec!["hub".to_string()]),
        other => panic!("expected the fallthrough to rank the hub, got {other:?}"),
    }
    // Local competed on the fallthrough and lost on score — it was
    // not skipped, and it was not preferred by fiat.
    let names: Vec<&str> = fell_through
        .candidates
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(names.contains(&"local"), "local must compete: {names:?}");
    assert!(names.contains(&"hub"), "hub must compete: {names:?}");

    // The outcome joins to the FALLTHROUGH record, because the ranked
    // scorer is what picked the server. Joining it to the named record
    // would attribute a serve to a decision that scored nothing.
    let outcome = await_outcome(&capture).await;
    assert_eq!(
        outcome.decision_id, fell_through.decision_id,
        "the outcome must join to the decision that actually chose"
    );
    match &outcome.served_by {
        ServedBy::Peer { name, .. } => assert_eq!(name, "hub"),
        other => panic!("expected peer service, got {other:?}"),
    }
}

/// The degrade path must survive the fallthrough: with nobody on the
/// mesh worth crossing to, an unavailable shared primary still serves
/// locally. This is the regression the fallthrough could plausibly
/// introduce — a request that used to end at local now taking a
/// pointless network hop, or failing outright.
#[tokio::test]
async fn forming_shared_model_with_no_worthy_peer_still_serves_locally() {
    let (provider, capture) = build(vec![dead_peer_endpoint("unreachable")]);
    provider.set_shared_model_id(Some(SHARED_PRIMARY.into()));

    let (mut stream, attribution) = provider
        .complete_stream_with_id(&mesh_request())
        .await
        .expect("no worthy peer must still produce an answer");
    assert!(
        !attribution.contains("@ peer"),
        "nothing on this mesh could serve it; got {attribution:?}"
    );
    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        body.push_str(&chunk.unwrap());
    }
    assert_eq!(body, "local answer");
    drop(stream);

    // Still recorded as a fallthrough that considered the mesh and
    // declined it — "the mesh lost" and "the mesh was never asked"
    // stay distinguishable.
    let fell_through = only_decision_on(&capture, DecisionPath::NamedFallthrough);
    assert!(
        matches!(fell_through.verdict, Verdict::StayLocal),
        "expected a scored stay-local, got {:?}",
        fell_through.verdict
    );
    assert_eq!(fell_through.excluded.len(), 1);
    assert_eq!(fell_through.excluded[0].name, "unreachable");
    assert_eq!(
        fell_through.excluded[0].reason,
        ExclusionReason::ManifestUnavailable
    );
}

/// The same rule on the non-streaming surface. Before this, `complete`
/// returned local the moment the shared primary was unavailable,
/// without ever consulting the scorer — and emitted no decision record
/// at all, so the loss was invisible.
#[tokio::test]
async fn non_streaming_complete_also_falls_through_to_the_mesh() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 6)]);
    provider.set_shared_model_id(Some(SHARED_PRIMARY.into()));

    // The mock peer speaks SSE only, so the transport attempt fails
    // and the cascade lands on local. What is under test is the
    // DECISION — that the scorer ran and chose the hub — not the mock's
    // ability to answer a non-streaming call.
    let _ = provider.complete(&mesh_request()).await;

    let fell_through = only_decision_on(&capture, DecisionPath::NamedFallthrough);
    match &fell_through.verdict {
        Verdict::Peers { ranked } => assert_eq!(ranked, &vec!["hub".to_string()]),
        other => panic!("expected the fallthrough to rank the hub, got {other:?}"),
    }
    let outcome = await_outcome(&capture).await;
    assert_eq!(outcome.decision_id, fell_through.decision_id);
}

/// The carve-out that keeps the fallthrough honest: a HARD named
/// target — an explicit `model_id` from the caller — is a constraint,
/// not a preference. It must still fail loudly rather than being
/// silently served by whatever the scorer likes. Silent substitution
/// was the original bug on this path; the soft fallthrough must not
/// reintroduce it.
#[tokio::test]
async fn hard_named_target_still_fails_loudly_rather_than_falling_through() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 6)]);

    let request = CompletionRequest {
        model_id: Some("a-model-nobody-has".into()),
        ..mesh_request()
    };
    // `expect_err` is unavailable here: the Ok half is a boxed stream,
    // which is not `Debug`.
    let err = match provider.complete_stream_with_id(&request).await {
        Ok(_) => panic!("an explicit model_id nobody advertises must be an error"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").contains("a-model-nobody-has"),
        "the error must name the model the caller asked for; got {err}"
    );

    // One record, on the named path. No fallthrough was attempted.
    let named = only_decision_on(&capture, DecisionPath::NamedModel);
    assert!(matches!(named.verdict, Verdict::NamedUnknown { .. }));
    assert!(
        !capture
            .decisions()
            .iter()
            .any(|d| d.path == DecisionPath::NamedFallthrough),
        "a hard target must never fall through to the scorer"
    );
}

// ── 6. F9: the local candidate is scored on real local load ──────

/// **F9's fix, pinned at the wire level.**
///
/// The defect: `load_penalty` (`scoring.rs:274`) reads `in_flight`
/// from the local candidate's `NodeObservations`, whose only mutator
/// is `record_dispatch(None)` — a method with **zero callers in the
/// repository**. The local candidate was therefore scored permanently
/// idle, `load_penalty` was a permanent 1.0, and the design comment
/// at `peer_inference.rs:1198` ("so a hot local slot can lose to an
/// idle peer on load") described behaviour the shipped code could not
/// exhibit. On a homogeneous fleet that makes the ranked path
/// structurally incapable of preferring a peer.
///
/// The fix reads `in_flight_publisher` — the RAII-maintained total
/// this node already gossips — at the gather point. This test drives
/// the real `MeshInferenceProvider` twice against the same mock peer,
/// changing nothing but that counter, and asserts the recorded
/// `ScoreBreakdown` moved. It fails on the pre-fix code with
/// `load_penalty` pinned at 1.0 in both runs, which is the whole
/// point: no existing test could tell the two states apart.
///
/// Asserted on the *record* rather than on the verdict because the
/// verdict is a threshold on top of the signal — a test that only
/// checked "did it offload" would pass for a fleet where the peer
/// wins anyway, and F9 is about the signal being absent, not about
/// any particular route.
#[tokio::test]
async fn the_local_candidate_is_scored_on_this_nodes_real_in_flight_count() {
    use std::sync::atomic::{AtomicU32, Ordering};

    async fn local_breakdown(in_flight: u32) -> (u32, f32, f32) {
        let addr = spawn_peer(false).await;
        let capture = Arc::new(CaptureDecisionSink::new());
        let sink: Arc<dyn DecisionSink> = capture.clone();
        let publisher = Arc::new(AtomicU32::new(0));
        let provider = MeshInferenceProvider::with_peer_source_and_publisher(
            weak_local(),
            Arc::new(StubPeerSource {
                peers: vec![peer_endpoint("hub", addr, 12)],
            }) as Arc<dyn PeerEndpointSource>,
            Arc::clone(&publisher),
        )
        .with_decision_sink(sink);

        // Stand in for N requests already on this node's slot. The
        // guards that normally maintain this counter are RAII and
        // drop at end of scope, so a test cannot hold them open
        // across a nested dispatch without deadlocking the harness.
        publisher.store(in_flight, Ordering::Relaxed);

        let (mut stream, _) = provider
            .complete_stream_with_id(&mesh_request())
            .await
            .expect("peer route should succeed");
        while let Some(chunk) = stream.next().await {
            let _ = chunk;
        }
        drop(stream);

        let decision = only_decision(&capture);
        let local = decision
            .candidates
            .iter()
            .find(|c| c.kind == CandidateKind::Local)
            .expect("the local candidate is always scored on the ranked path");
        (
            local.inputs.in_flight,
            local.score.load_penalty,
            local.score.final_score,
        )
    }

    let (idle_n, idle_penalty, idle_score) = local_breakdown(0).await;
    let (busy_n, busy_penalty, busy_score) = local_breakdown(8).await;

    assert_eq!(
        idle_n, 0,
        "an idle node must record in_flight 0 for itself"
    );
    assert_eq!(
        busy_n, 8,
        "the local candidate's recorded in_flight must be this node's real count, \
         not the never-written `local_observations` field (F9). Got {busy_n}."
    );
    assert!(
        (idle_penalty - 1.0).abs() < 1e-6,
        "an idle local slot must carry no load penalty, got {idle_penalty}"
    );
    // `load_penalty` is `1 / (1 + 0.05 n)`; at n = 8 that is 1/1.4.
    let expected = 1.0f32 / 1.4;
    assert!(
        (busy_penalty - expected).abs() < 1e-4,
        "8 in flight must penalise the local slot to {expected:.4}, got {busy_penalty:.4} \
         — if this is 1.0 the scorer is reading the counter nothing writes"
    );
    assert!(
        busy_score < idle_score,
        "a loaded local slot must score below an idle one ({busy_score} vs {idle_score}) \
         — this is the property `peer_inference.rs:1198` has always claimed"
    );
}

// ── The NON-STREAMING named path ────────────────────────────────
//
// Added 2026-08-06. `complete()` carried its own inline copy of the
// named-model routing logic and never called `select_route`, so it
// applied neither the forward budget nor the decision record. The
// hole was found by arming `SOVEREIGN_DECISION_LOG` on a live daemon
// and watching an identical peer-routed request emit 2 records when
// streamed and 0 when not.
//
// Both tests below fail against the pre-fix build: the first with "no
// decision record", the second because the request is forwarded to the
// peer despite an exhausted budget.

fn named_request(model_id: &str) -> CompletionRequest {
    CompletionRequest {
        model_id: Some(model_id.to_string()),
        ..CompletionRequest::new("Say OK").with_speed(Speed::Slow)
    }
}

#[tokio::test]
async fn non_streaming_named_dispatch_emits_a_joined_decision_and_outcome() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("peer-a", addr, 1)]);

    provider
        .complete(&named_request("Qwen3.5-9B.test"))
        .await
        .expect("peer should serve the named model");

    let decision = only_decision(&capture);
    assert!(
        matches!(decision.verdict, Verdict::NamedPeer { .. }),
        "the non-streaming named path must record WHERE it sent the request; \
         got {:?}",
        decision.verdict
    );
    let outcome = await_outcome(&capture).await;
    assert_eq!(
        outcome.decision_id, decision.decision_id,
        "every outcome must join back to its decision, on BOTH routing surfaces"
    );
}

#[tokio::test]
async fn non_streaming_named_dispatch_refuses_to_forward_an_exhausted_request() {
    // THE CORRECTNESS HALF. A request that some other node already
    // forwarded carries a spent budget. Forwarding it again is the
    // ping-pong M1 exists to close — and until this fix, the
    // non-streaming path did exactly that, because `build_request`
    // SPENDS the budget on every hop but nothing on this path ever
    // READ it.
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("peer-a", addr, 1)]);

    let already_forwarded = CompletionRequest {
        model_id: Some("Qwen3.5-9B.test".to_string()),
        ..CompletionRequest::new("Say OK")
            .with_speed(Speed::Slow)
            .with_oicp(InferenceRequirements::new().with_forward_budget(0))
    };

    // The local stub does not hold `Qwen3.5-9B.test`, so the honest
    // downgrade is Unknown — a loud refusal, never a silent second hop.
    let err = provider
        .complete(&already_forwarded)
        .await
        .expect_err("an already-forwarded named request must not be forwarded again");
    let msg = err.to_string();
    assert!(
        msg.contains("Qwen3.5-9B.test"),
        "the refusal must name the model it could not place: {err}"
    );
    // B1 (measured 2026-08-06, M6-B): this refusal used to claim "no node in
    // this mesh advertises model X — check `/v1/models`", which is FALSE here
    // — peer-a advertises it, which is the only reason the budget gate had a
    // Peer to downgrade. An operator following that instruction found the
    // model listed and had nowhere to go. The cause is the hop budget, so the
    // message must say so, and must NOT say the other thing.
    assert!(
        msg.contains("forwarded") && msg.contains("budget"),
        "the refusal must name the HOP BUDGET as the cause, since a peer does \
         advertise this model: {err}"
    );
    assert!(
        !msg.contains("no node in this mesh advertises"),
        "the refusal must not claim the mesh lacks a model a peer is \
         advertising — that is the B1 dead end: {err}"
    );

    let decision = only_decision(&capture);
    assert!(
        matches!(decision.verdict, Verdict::NamedUnknown { .. }),
        "an exhausted budget must downgrade the peer to Unknown, not dispatch; \
         got {:?}",
        decision.verdict
    );
}

#[tokio::test]
async fn a_streaming_refusal_still_joins_an_outcome_to_its_decision() {
    // C2, measured 2026-08-06: the STREAMING refusal arm returned Err bare, so
    // three refusals in the M6-C run left three `NamedUnknown` decisions with
    // no outcome. Anyone counting outcomes-per-decision out of the decision log
    // saw phantom un-joined decisions for exactly the event they were looking
    // for. The non-streaming path had already fixed this; this pins the pair on
    // BOTH surfaces so they cannot drift again.
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("peer-a", addr, 1)]);

    let mut stream = match provider
        .complete_stream(&named_request("Nonexistent-99B.test"))
        .await
    {
        Ok(_) => panic!("a model no node advertises must not produce a stream"),
        Err(e) => {
            assert!(
                e.to_string().contains("no node in this mesh advertises"),
                "expected the absence refusal, got: {e}"
            );
            None::<()>
        }
    };
    let _ = stream.take();

    let decision = only_decision(&capture);
    assert!(
        matches!(decision.verdict, Verdict::NamedUnknown { .. }),
        "expected a NamedUnknown decision; got {:?}",
        decision.verdict
    );
    let outcome = await_outcome(&capture).await;
    assert_eq!(
        outcome.decision_id, decision.decision_id,
        "a streaming refusal must join an outcome to its decision — that is the \
         whole of C2"
    );
}

#[tokio::test]
async fn a_local_only_envelope_does_not_cross_the_trust_boundary() {
    // B2, measured 2026-08-06: this used to be served BY THE PEER, 200.
    // The privacy gate lives in `offload_verdict`, which named dispatch
    // never reaches, and routes_inference's forwarding-boundary gate sits
    // AFTER the provider that does the forwarding — so nothing stopped it.
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("peer-a", addr, 1)]);

    // A full forward budget, so the hop bound CANNOT be what refuses this —
    // privacy has to be the thing that fires, or the test proves nothing.
    let local_only = CompletionRequest {
        model_id: Some("Qwen3.5-9B.test".to_string()),
        ..CompletionRequest::new("Say OK")
            .with_speed(Speed::Slow)
            .with_oicp(
                InferenceRequirements::new()
                    .with_forward_budget(1)
                    .with_sharding(ShardingPrivacy::LocalOnly),
            )
    };

    let err = provider
        .complete(&local_only)
        .await
        .expect_err("a local_only named request must not be served by a peer");
    let msg = err.to_string();
    assert!(
        msg.contains("local_only"),
        "the refusal must name PRIVACY as the cause, not absence or the hop \
         budget: {err}"
    );
    assert!(
        !msg.contains("budget"),
        "privacy must not be misreported as budget exhaustion — the request \
         had a full budget: {err}"
    );

    let decision = only_decision(&capture);
    assert!(
        matches!(decision.verdict, Verdict::NamedUnknown { .. }),
        "a local_only envelope must refuse, never dispatch to a peer; got {:?}",
        decision.verdict
    );
}

#[tokio::test]
async fn a_thin_client_with_no_envelope_still_reaches_a_peer() {
    // THE REGRESSION GUARD for the fix above, and the more important half.
    // This module's rule 1 once read "No OICP on the request, OR sharding ==
    // LocalOnly -> local". Implemented literally, this request — an IDE or any
    // OpenAI client that pins `model` and knows nothing about OICP — would be
    // refused for a model only a peer holds, which is exactly the consumer
    // story M6-A proved works. An absent envelope states NOTHING; only a
    // present one that withholds `mesh_allowed` is an opt-out.
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("peer-a", addr, 1)]);

    provider
        .complete(&named_request("Qwen3.5-9B.test"))
        .await
        .expect("a named request with NO envelope must still reach the peer");

    let decision = only_decision(&capture);
    assert!(
        matches!(decision.verdict, Verdict::NamedPeer { .. }),
        "no envelope means no stated privacy — the peer must still serve it; \
         got {:?}",
        decision.verdict
    );
}

#[tokio::test]
async fn a_mesh_allowed_envelope_crosses_the_boundary_as_asked() {
    // The third arm: an explicit opt-in must behave exactly like the
    // envelope-less case. Without this, a gate that refused EVERY
    // envelope-bearing request would pass the two tests above.
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("peer-a", addr, 1)]);

    let opted_in = CompletionRequest {
        model_id: Some("Qwen3.5-9B.test".to_string()),
        ..CompletionRequest::new("Say OK")
            .with_speed(Speed::Slow)
            .with_oicp(
                InferenceRequirements::new()
                    .with_forward_budget(1)
                    .with_sharding(ShardingPrivacy::MeshAllowed),
            )
    };

    provider
        .complete(&opted_in)
        .await
        .expect("mesh_allowed is an explicit opt-in — the peer must serve it");

    let decision = only_decision(&capture);
    assert!(
        matches!(decision.verdict, Verdict::NamedPeer { .. }),
        "mesh_allowed must route to the peer; got {:?}",
        decision.verdict
    );
}

#[tokio::test]
async fn a_genuinely_absent_model_still_says_nobody_advertises_it() {
    // THE CONTRAST that makes the test above mean something. Both causes
    // end in `NamedModelLocation::Unknown` and the same 503, so pinning
    // only the hop-exhausted wording would be satisfied by a message that
    // says "hop budget" unconditionally — including when the mesh really
    // does not have the model. One reason per cause, or the distinction
    // the enum exists for is untested (§18.1: name the failing input).
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("peer-a", addr, 1)]);

    // A full budget, so the hop gate cannot fire — and an id no node
    // advertises, so the honest answer is absence.
    let err = provider
        .complete(&named_request("Nonexistent-99B.test"))
        .await
        .expect_err("a model no node advertises must be refused, never substituted");
    let msg = err.to_string();
    assert!(
        msg.contains("no node in this mesh advertises"),
        "genuine absence must still be reported as absence: {err}"
    );
    assert!(
        !msg.contains("budget"),
        "absence must NOT be blamed on the hop budget — the inverse of B1 is \
         just as misleading: {err}"
    );

    let decision = only_decision(&capture);
    assert!(
        matches!(decision.verdict, Verdict::NamedUnknown { .. }),
        "an unadvertised model is Unknown, not a substitution; got {:?}",
        decision.verdict
    );
}

// ═══════════════════════════════════════════════════════════════
// CHARACTERIZATION — the non-streaming RANKED path.
//
// Written BEFORE unifying `complete()` onto `select_route`'s plan
// (ARCH §10.4: land the test first when the code you are about to
// move has none). The coverage audit found this path's OUTCOME side
// entirely unpinned: `non_streaming_complete_closes_the_join` above
// discards its result with `let _ =`, so nothing asserted who
// actually served a ranked non-streaming turn.
//
// These three pin behaviour that the unification must PRESERVE. The
// one thing it deliberately changes — trying a SECOND ranked peer
// instead of collapsing to local after the first — is pinned
// separately, below, because it cannot pass before the change.
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_ranked_non_streaming_turn_is_served_by_the_peer_and_attributed_to_it() {
    let addr = spawn_peer(false).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 6)]);

    let resp = provider
        .complete(&mesh_request())
        .await
        .expect("the peer serves non-streaming JSON");

    assert_eq!(resp.text, PEER_TEXT, "the PEER's answer, not the local stub's");
    assert!(
        resp.model_id.contains("@ peer hub"),
        "a peer-served turn must be attributed to the peer; got {:?}",
        resp.model_id
    );

    let decision = only_decision(&capture);
    let outcome = await_outcome(&capture).await;
    assert_eq!(outcome.decision_id, decision.decision_id);
    assert!(
        matches!(outcome.served_by, ServedBy::Peer { .. }),
        "got {:?}",
        outcome.served_by
    );
    assert_eq!(outcome.attempt_index, 0, "served first try");
    assert!(outcome.failovers.is_empty());
}

#[tokio::test]
async fn a_ranked_non_streaming_turn_with_no_worthy_peer_stays_local() {
    // No peers at all: `select_peer` finds nobody, so this node serves
    // without ever attempting a hop — and must record that it did so
    // with an EMPTY failover list. A failover list that grows here
    // would mean we invented an attempt that never happened.
    let (provider, capture) = build(vec![]);

    let resp = provider
        .complete(&mesh_request())
        .await
        .expect("with no peer, the local provider answers");

    assert_eq!(resp.text, "local answer");

    let decision = only_decision(&capture);
    let outcome = await_outcome(&capture).await;
    assert_eq!(outcome.decision_id, decision.decision_id);
    assert!(
        matches!(outcome.served_by, ServedBy::LocalFallback { .. }),
        "got {:?}",
        outcome.served_by
    );
    assert_eq!(outcome.attempt_index, 0);
    assert!(
        outcome.failovers.is_empty(),
        "no peer was tried, so no failover may be recorded: {:?}",
        outcome.failovers
    );
}

#[tokio::test]
async fn a_shedding_ranked_peer_falls_back_to_local_on_the_non_streaming_path() {
    // The non-streaming twin of
    // `a_shedding_peer_leaves_a_failover_attempt_on_the_outcome`,
    // which only ever covered the streaming surface. With ONE peer the
    // cascade is the same shape before and after the unification, so
    // this holds across the change.
    let addr = spawn_peer(true).await;
    let (provider, capture) = build(vec![peer_endpoint("hub", addr, 9)]);

    let resp = provider
        .complete(&mesh_request())
        .await
        .expect("a shed peer must fall back to local, not fail the caller");

    assert_eq!(resp.text, "local answer");
    assert!(
        !resp.model_id.contains("@ peer"),
        "a shedding peer must not be attributed; got {:?}",
        resp.model_id
    );

    let outcome = await_outcome(&capture).await;
    assert_eq!(outcome.attempt_index, 1, "serving from step 1 = one failover");
    assert!(matches!(outcome.served_by, ServedBy::LocalFallback { .. }));
    assert_eq!(outcome.failovers.len(), 1);
    assert_eq!(outcome.failovers[0].peer, "hub");
    assert!(
        outcome.failovers[0].shed,
        "a 503 is a shed, not a transport failure: {:?}",
        outcome.failovers[0]
    );
}
