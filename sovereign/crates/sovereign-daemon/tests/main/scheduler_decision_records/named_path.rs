// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the scheduler_decision_records suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! Named-model routing: the soft shared-primary target that falls
//! through to the ranked mesh (and its hard-target carve-out), and
//! the non-streaming named path — dispatch, budget refusal, privacy
//! boundary and absence wording.

use futures::StreamExt;
use oicp_types::{InferenceRequirements, ShardingPrivacy};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, Speed};
use sovereign_mesh::decision_log::{
    CaptureDecisionSink, DecisionPath, ExclusionReason, RoutingDecision, ServedBy, Verdict,
};

use super::{
    await_outcome, build, dead_peer_endpoint, mesh_request, only_decision, peer_endpoint,
    spawn_peer, PEER_TEXT,
};

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
