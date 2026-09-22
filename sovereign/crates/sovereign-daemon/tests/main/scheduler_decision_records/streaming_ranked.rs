// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the scheduler_decision_records suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! Sections 1–4 of the phase-0 proof: the join closes on the streaming
//! peer path, provenance is real, non-selection is recorded, failover
//! is visible.

use futures::StreamExt;
use oicp_types::{CapabilityHint, InferenceRequirements, LatencyClass, ShardingPrivacy};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, Speed};
use sovereign_mesh::decision_log::{
    CandidateKind, DecisionPath, ExclusionReason, LoadSource, ServedBy, Verdict,
};

use super::{
    await_outcome, build, dead_peer_endpoint, mesh_request, only_decision, peer_endpoint,
    spawn_peer, PEER_TEXT,
};

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
    let names: Vec<&str> = decision
        .candidates
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        names.contains(&"local"),
        "local must be recorded: {names:?}"
    );
    assert!(names.contains(&"hub"), "peer must be recorded: {names:?}");

    let hub = decision
        .candidates
        .iter()
        .find(|c| c.name == "hub")
        .unwrap();
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
    let hub = decision
        .candidates
        .iter()
        .find(|c| c.name == "hub")
        .unwrap();
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
    let bench_age = inputs
        .bench_age_secs
        .expect("benchmark age must be stamped");
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
    let request = CompletionRequest::new("private")
        .with_speed(Speed::Slow)
        .with_oicp(
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
