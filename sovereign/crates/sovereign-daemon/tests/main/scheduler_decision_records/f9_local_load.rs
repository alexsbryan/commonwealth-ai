// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the scheduler_decision_records suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! F9: the local candidate is scored on this node's real in-flight
//! count, read at the gather point from the RAII-maintained publisher.

use std::sync::Arc;

use futures::StreamExt;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_mesh::decision_log::{CandidateKind, CaptureDecisionSink, DecisionSink};
use sovereign_mesh::peer_inference::{InferenceRouter, VenueHost, VenueSource};

use super::{mesh_request, only_decision, peer_endpoint, spawn_peer, weak_local, StubVenueSource};

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
/// the real `InferenceRouter` twice against the same mock peer,
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
        let provider = InferenceRouter::with_peer_source_and_publisher(
            weak_local(),
            Arc::new(StubVenueSource {
                peers: vec![peer_endpoint("hub", addr, 12)],
            }) as Arc<dyn VenueSource>,
            Arc::new(StubVenueSource {
                peers: vec![peer_endpoint("hub", addr, 12)],
            }) as Arc<dyn VenueHost>,
            Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
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

    assert_eq!(idle_n, 0, "an idle node must record in_flight 0 for itself");
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
