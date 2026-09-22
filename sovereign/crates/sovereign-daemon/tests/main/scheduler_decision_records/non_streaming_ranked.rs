// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the scheduler_decision_records suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! The non-streaming ranked surface: the join closes on `complete()`
//! too, and the characterization pins written before unifying
//! `complete()` onto `select_route`'s plan.

use sovereign_contracts::traits::InferenceProvider;
use sovereign_mesh::decision_log::ServedBy;

use super::{
    await_outcome, build, mesh_request, only_decision, peer_endpoint, spawn_peer, PEER_TEXT,
};

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

    assert_eq!(
        resp.text, PEER_TEXT,
        "the PEER's answer, not the local stub's"
    );
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
    assert_eq!(
        outcome.attempt_index, 1,
        "serving from step 1 = one failover"
    );
    assert!(matches!(outcome.served_by, ServedBy::LocalFallback { .. }));
    assert_eq!(outcome.failovers.len(), 1);
    assert_eq!(outcome.failovers[0].peer, "hub");
    assert!(
        outcome.failovers[0].shed,
        "a 503 is a shed, not a transport failure: {:?}",
        outcome.failovers[0]
    );
}

/// THE BEHAVIOUR THE UNIFICATION ADDS. Before `complete()` consumed
/// `select_route`'s plan it used `select_peer` — one pick, and if that
/// peer declined, straight to local. The streaming surface had walked
/// the full ranked cascade since it was written. Two shedding peers
/// now produce TWO failover attempts on the non-streaming path, not
/// one, which is the whole reason a second peer is worth ranking.
///
/// This test cannot pass against the pre-unification body.
#[tokio::test]
async fn a_ranked_non_streaming_turn_tries_every_worthy_peer_before_going_local() {
    let first = spawn_peer(true).await;
    let second = spawn_peer(true).await;
    let (provider, capture) = build(vec![
        peer_endpoint("hub", first, 9),
        peer_endpoint("spare", second, 9),
    ]);

    let resp = provider
        .complete(&mesh_request())
        .await
        .expect("both peers shed, so local answers");

    assert_eq!(resp.text, "local answer");

    let outcome = await_outcome(&capture).await;
    assert_eq!(
        outcome.failovers.len(),
        2,
        "both ranked peers must be attempted before collapsing to local — \
         one attempt means the cascade stopped at the first decline: {:?}",
        outcome.failovers
    );
    let tried: Vec<&str> = outcome.failovers.iter().map(|f| f.peer.as_str()).collect();
    assert!(
        tried.contains(&"hub") && tried.contains(&"spare"),
        "got {tried:?}"
    );
    assert!(
        outcome.failovers.iter().all(|f| f.shed),
        "both declines were 503s and must be classified as sheds: {:?}",
        outcome.failovers
    );
    assert_eq!(outcome.attempt_index, 2, "local served from step 2");
    assert!(matches!(outcome.served_by, ServedBy::LocalFallback { .. }));
}
