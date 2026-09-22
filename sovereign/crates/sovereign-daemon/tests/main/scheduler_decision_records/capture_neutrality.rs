// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the scheduler_decision_records suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! Sections 6–8: a capture round-trips into a replayable fixture,
//! instrumentation must not change routing, and snapshot cadence is
//! rate-limited rather than per-request.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_mesh::decision_log::{DecisionEvent, DecisionSink};
use sovereign_mesh::decision_trace::SchedulerTrace;
use sovereign_mesh::peer_inference::{InferenceRouter, VenueHost, VenueSource};
use sovereign_serving_host::recorder::TracingDecisionSink;

use super::{build, mesh_request, peer_endpoint, spawn_peer, weak_local, StubVenueSource};

// ── 6. A capture round-trips into a replayable fixture ──────────

#[tokio::test]
async fn jsonl_capture_loads_back_as_a_replayable_trace() {
    let addr = spawn_peer(false).await;
    let dir = std::env::temp_dir().join(format!("sched-trace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("decisions.jsonl");

    let sink: Arc<dyn DecisionSink> = Arc::new(TracingDecisionSink::to_path(&path).unwrap());
    let provider = InferenceRouter::with_peer_source(
        weak_local(),
        Arc::new(StubVenueSource {
            peers: vec![peer_endpoint("hub", addr, 14)],
        }) as Arc<dyn VenueSource>,
        Arc::new(StubVenueSource {
            peers: vec![peer_endpoint("hub", addr, 14)],
        }) as Arc<dyn VenueHost>,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
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
    let silent = InferenceRouter::with_peer_source(
        weak_local(),
        Arc::new(StubVenueSource {
            peers: vec![peer_endpoint("hub", addr, 11)],
        }) as Arc<dyn VenueSource>,
        Arc::new(StubVenueSource {
            peers: vec![peer_endpoint("hub", addr, 11)],
        }) as Arc<dyn VenueHost>,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
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
