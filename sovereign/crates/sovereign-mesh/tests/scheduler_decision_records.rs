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

async fn chat_completions_handler() -> impl IntoResponse {
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
