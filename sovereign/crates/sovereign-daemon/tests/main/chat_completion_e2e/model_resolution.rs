// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the chat_completion_e2e e2e suite — split from chat_completion_e2e.rs for the §3.2 size ceiling (behaviour-preserving move).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Query;
use axum::response::{sse::Event, IntoResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::ids::NodeId;
use oicp_types::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus,
    ProviderManifest, ProviderModel,
};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, Speed};
use sovereign_daemon::daemon::InferenceVenue;
use sovereign_mesh::peer_inference::InferenceRouter;

use super::{
    drain, local_byom, mip_with_peers, two_slot_manifest, StreamQuery, PEER_RESPONSE_TEXT,
};

// ── Model-resolving mock peer — serviceability, not just transport ──
//
// `chat_completions_handler` above accepts ANY body and never looks at
// `model`. That is why twelve passing tests in this file coexisted with a
// total outage of anonymous peer offload for weeks: every unnamed
// Normal/Extended dispatch carried the unservable placeholder
// `"mesh-peer"`, and a mock that never resolves a name cannot notice.
//
// This peer resolves `model` the way a receiving daemon does:
//
//   * a non-empty `model` it does not advertise → the 503 a real node
//     returns when `locate_named_model` yields `Unknown` and the request
//     becomes `Error::ModelNotLoaded` (`peer_inference.rs:1783-1787`);
//   * an empty `model` → route on the OICP envelope, which is what the
//     ranked path intends. No envelope either is a 400, because the
//     request then carries neither a resolvable name nor a routing
//     opinion and no receiver could do anything with it.
//
// The servable set is derived FROM the advertised manifest rather than
// listed separately, so the two cannot drift apart — a mock whose
// "what I serve" and "what I advertise" disagree is how you get a green
// suite over a broken fleet.

/// What the resolving peer did, in arrival order.
#[derive(Default)]
struct VenueLedger {
    bodies: Vec<serde_json::Value>,
    /// Requests this peer actually generated tokens for.
    served: usize,
    /// Requests refused because `model` named something it does not
    /// advertise — the outage signature.
    refused_unresolvable: usize,
}

type VenueLedgerHandle = Arc<std::sync::Mutex<VenueLedger>>;

struct ResolvingVenueState {
    manifest: ProviderManifest,
    advertised: Vec<String>,
    ledger: VenueLedgerHandle,
}

/// `two_slot_manifest` plus the `primary` / `commonwealth/primary` alias
/// rows a real node emits off its Slow slot
/// (`oicp_synthesis.rs:149-195`). The soak probe drives the named-alias
/// class, so the in-process peer advertises the same shape.
fn aliased_manifest() -> ProviderManifest {
    let mut manifest = two_slot_manifest(Vec::new());
    for alias in ["commonwealth/primary", "primary"] {
        manifest.models.push(ProviderModel {
            id: alias.into(),
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
                LatencyClass::Normal,
                32_768,
                4_000,
                0.80,
            )],
            fingerprint: None,
        });
    }
    manifest
}

fn canned_sse_response() -> axum::response::Response {
    let delta = |s: &str| {
        serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": { "content": s },
                "finish_reason": null,
            }]
        })
        .to_string()
    };
    let (first, second) = PEER_RESPONSE_TEXT.split_at(PEER_RESPONSE_TEXT.len() / 2);
    let events = vec![
        Ok::<_, std::convert::Infallible>(Event::default().data(delta(first))),
        Ok(Event::default().data(delta(second))),
        Ok(Event::default().data("[DONE]")),
    ];
    Sse::new(futures::stream::iter(events)).into_response()
}

async fn resolving_capabilities_handler(
    axum::extract::State(state): axum::extract::State<Arc<ResolvingVenueState>>,
) -> impl IntoResponse {
    Json(state.manifest.clone())
}

async fn resolving_chat_handler(
    axum::extract::State(state): axum::extract::State<Arc<ResolvingVenueState>>,
    Query(_q): Query<StreamQuery>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    let model = body["model"].as_str().unwrap_or("").trim().to_string();
    let has_envelope = body.get("oicp").is_some();
    let unresolvable = !model.is_empty() && !state.advertised.iter().any(|id| *id == model);
    let anonymous_without_envelope = model.is_empty() && !has_envelope;

    {
        let mut ledger = state.ledger.lock().expect("ledger poisoned");
        ledger.bodies.push(body.clone());
        if unresolvable {
            ledger.refused_unresolvable += 1;
        } else if !anonymous_without_envelope {
            ledger.served += 1;
        }
    }

    if unresolvable {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "message": format!("no node in this mesh advertises model '{model}'"),
                    "type": "model_not_loaded",
                }
            })),
        )
            .into_response();
    }
    if anonymous_without_envelope {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": { "message": "anonymous request carried no OICP envelope" }
            })),
        )
            .into_response();
    }
    // Answer the shape the caller actually asked for. This mock spoke
    // only SSE until 2026-08-07, which is why no test had ever driven
    // `complete()` (non-streaming) against a resolving peer — the
    // request "succeeded" into an unparseable body, the cascade fell
    // back to local, and the assertion that would have caught it did
    // not exist. Same trap `scheduler_decision_records.rs` records.
    let streaming = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !streaming {
        return Json(serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": PEER_RESPONSE_TEXT },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12 }
        }))
        .into_response();
    }
    canned_sse_response()
}

async fn spawn_resolving_peer() -> (SocketAddr, VenueLedgerHandle) {
    let manifest = aliased_manifest();
    let advertised: Vec<String> = manifest.models.iter().map(|m| m.id.clone()).collect();
    let ledger: VenueLedgerHandle = Arc::new(std::sync::Mutex::new(VenueLedger::default()));
    let state = Arc::new(ResolvingVenueState {
        manifest,
        advertised,
        ledger: Arc::clone(&ledger),
    });
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(resolving_capabilities_handler))
        .route("/v1/chat/completions", post(resolving_chat_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, ledger)
}

fn founder_endpoint(addr: SocketAddr) -> InferenceVenue {
    InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![format!("http://{}/v1", addr)],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }
}

fn mesh_allowed_envelope() -> InferenceRequirements {
    InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed)
}

fn provider_with_resolving_peer(addr: SocketAddr) -> InferenceRouter {
    mip_with_peers(local_byom(), vec![founder_endpoint(addr)])
}

/// The instrument itself must discriminate. Asserted directly against the
/// mock rather than through the scheduler, because every serviceability
/// claim below rests on this: a permissive mock would let all three
/// dispatch-class tests pass vacuously, which is precisely the failure
/// mode that let `"mesh-peer"` survive twelve green e2e tests.
#[tokio::test]
async fn the_resolving_peer_refuses_a_model_it_does_not_advertise() {
    let (addr, ledger) = spawn_resolving_peer().await;
    let client = reqwest::Client::new();

    let refused = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "mesh-peer",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .expect("mock peer should answer");
    assert_eq!(
        refused.status().as_u16(),
        503,
        "an unadvertised model must draw the same 503 a real daemon returns \
         from locate_named_model → ModelNotLoaded"
    );

    let accepted = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "Qwen3.5-9B.test",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .expect("mock peer should answer");
    assert_eq!(
        accepted.status().as_u16(),
        200,
        "an advertised model must be served — otherwise the mock refuses \
         everything and proves nothing"
    );

    let ledger = ledger.lock().expect("ledger poisoned");
    assert_eq!(ledger.refused_unresolvable, 1);
    assert_eq!(ledger.served, 1);
}

/// Class A — named dispatch. A `model` the peer advertises and the local
/// side does not must be SERVED by the peer, not merely accepted by it.
///
/// Deliberately uses a peer-only id rather than the `primary` alias: the
/// local stub's `model_id_for` returns its id for every speed, so
/// `build_self_manifest` advertises `primary` locally too, and
/// `locate_named_model`'s in-flight tiebreak (`peer_inference.rs:1501`)
/// then keeps an idle origin local — correctly. Driving the alias class
/// needs a non-zero `local_inflight_by_model`, which only real
/// concurrency produces; that is the soak probe's job, not this test's.
#[tokio::test]
async fn a_named_dispatch_is_served_by_the_peer_that_advertises_it() {
    let (addr, ledger) = spawn_resolving_peer().await;
    let wrapper = provider_with_resolving_peer(addr);

    let request = CompletionRequest::new("hi")
        .with_speed(Speed::Slow)
        .with_model_id("Qwen3.5-9B.test");

    let (stream, attribution) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("a named dispatch for a peer-advertised model must reach the peer");
    let text = drain(stream).await;

    assert_eq!(
        text, PEER_RESPONSE_TEXT,
        "the tokens must have come from the peer, not a local fallback"
    );
    assert!(
        attribution.contains("@ peer Founder"),
        "attribution must name the serving peer; got {attribution:?}"
    );

    let ledger = ledger.lock().expect("ledger poisoned");
    assert_eq!(ledger.served, 1, "peer served exactly one request");
    assert_eq!(
        ledger.refused_unresolvable, 0,
        "the dispatch named something the peer could not resolve: {:?}",
        ledger.bodies
    );
}

/// Class B — ranked anonymous. The class that was totally broken.
///
/// This is the test that would have caught `"mesh-peer"` on day one:
/// against a peer that resolves `model`, an unnamed dispatch carrying the
/// placeholder draws a 503, the cascade exhausts, and the request lands on
/// a local fallback that cannot answer — so the assertion below fails
/// loudly instead of certifying a transport that serves nobody.
#[tokio::test]
async fn an_anonymous_ranked_dispatch_is_actually_served_by_the_peer() {
    let (addr, ledger) = spawn_resolving_peer().await;
    let wrapper = provider_with_resolving_peer(addr);

    // model_id LEFT UNSET — the only shape `build_completion_request`
    // (`inference_adapter.rs:324-329`) produces for an inbound chat that
    // pins no model.
    let request = CompletionRequest::new("Summarise the argument for compatibilism.")
        .with_speed(Speed::Slow)
        .with_oicp(mesh_allowed_envelope());

    let (stream, attribution) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("an anonymous ranked dispatch must be servable by the peer");
    let text = drain(stream).await;

    assert_eq!(
        text, PEER_RESPONSE_TEXT,
        "the peer must have generated the tokens; a local fallback here means \
         the dispatch was unservable"
    );
    assert!(
        attribution.contains("@ peer Founder"),
        "attribution must name the serving peer; got {attribution:?}"
    );

    let ledger = ledger.lock().expect("ledger poisoned");
    assert_eq!(
        ledger.refused_unresolvable, 0,
        "the peer refused the dispatch as unresolvable — this is the \
         'mesh-peer' outage signature. Bodies: {:?}",
        ledger.bodies
    );
    assert_eq!(ledger.served, 1, "peer served exactly one request");
}

/// Class C — shared primary (soft named). Pins CURRENT behaviour,
/// including a known gap it does not fix.
///
/// `select_route` resolves the shared target into a local variable
/// (`peer_inference.rs:1665-1668`) but the streaming dispatch at `:2601`
/// sends the UNTOUCHED request, so the shared model id never reaches the
/// wire — the peer serves off the envelope instead. That is honest routing
/// but not target-honouring: a request for a 122B shared primary can land
/// on a peer's 35B. Non-streaming `complete` does it correctly
/// (`:2086-2092` builds an owned copy).
///
/// Fixing it means deciding whether to pin `peer_cand.model_id` on
/// dispatch, which also changes RANKED semantics — a design call. So this
/// test asserts the gap rather than papering over it: if someone closes
/// it, this test fails and they update it deliberately.
#[tokio::test]
async fn a_shared_primary_reaches_the_peer_but_does_not_yet_pin_its_target() {
    let (addr, ledger) = spawn_resolving_peer().await;
    let wrapper = provider_with_resolving_peer(addr);
    // Advertised by the peer, not by the local stub.
    wrapper.set_shared_model_id(Some("Qwen3.5-27B.test".into()));

    let request = CompletionRequest::new("hi")
        .with_speed(Speed::Slow)
        .with_oicp(mesh_allowed_envelope());

    let (stream, attribution) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("a shared-primary request must reach the mesh");
    let text = drain(stream).await;

    assert_eq!(text, PEER_RESPONSE_TEXT, "the peer must have served it");
    assert!(
        attribution.contains("@ peer Founder"),
        "attribution must name the serving peer; got {attribution:?}"
    );

    let ledger = ledger.lock().expect("ledger poisoned");
    assert_eq!(ledger.served, 1);
    assert_eq!(
        ledger.refused_unresolvable, 0,
        "bodies: {:?}",
        ledger.bodies
    );

    // GAP CLOSED 2026-08-07, deliberately, per this assertion's own
    // former instruction. Unifying `complete()` onto `select_route`
    // moved "which model goes on the wire" onto the route step
    // (`RouteDecision::Peer::pinned_model_id`), and once it was a
    // property of the DECISION rather than of one hand-written body,
    // both surfaces got it. Previously this asserted the wire model
    // was EMPTY and the peer routed on the envelope instead.
    let wire_model = ledger.bodies[0]["model"].as_str().unwrap_or("<missing>");
    assert_eq!(
        wire_model, "Qwen3.5-27B.test",
        "a shared-primary route must pin the model it resolved, or a peer that \
         resolves strictly refuses the turn and the cascade silently serves the \
         caller something else. Got {wire_model:?}"
    );
}

/// The NON-STREAMING twin of
/// `a_shared_primary_reaches_the_peer_but_does_not_yet_pin_its_target`.
///
/// Written while unifying `complete()` onto `select_route`, because
/// the coverage audit found the shared-primary rewrite had NO test on
/// this surface at all — and the old inline body did pin the resolved
/// id onto the outgoing request (`_shared_owned`) where the streaming
/// body does not. A whole test suite going green says nothing about a
/// behaviour nothing asserts (§18.1), so this asserts it.
#[tokio::test]
async fn a_shared_primary_non_streaming_turn_reaches_the_peer() {
    let (addr, ledger) = spawn_resolving_peer().await;
    let wrapper = provider_with_resolving_peer(addr);
    wrapper.set_shared_model_id(Some("Qwen3.5-27B.test".into()));

    let request = CompletionRequest::new("hi")
        .with_speed(Speed::Slow)
        .with_oicp(mesh_allowed_envelope());

    let resp = wrapper
        .complete(&request)
        .await
        .expect("a shared-primary request must reach the mesh");

    assert_eq!(
        resp.text, PEER_RESPONSE_TEXT,
        "the peer must have served it"
    );
    assert!(
        resp.model_id.contains("@ peer Founder"),
        "attribution must name the serving peer; got {:?}",
        resp.model_id
    );

    let ledger = ledger.lock().expect("ledger poisoned");
    assert_eq!(ledger.served, 1);
    assert_eq!(
        ledger.refused_unresolvable, 0,
        "the peer must not have been asked for a model it cannot resolve; bodies: {:?}",
        ledger.bodies
    );

    // THE DELTA THIS TEST EXISTS TO MEASURE. Record what actually goes
    // on the wire; the assertion below states which of the two
    // behaviours is current, so a change here is never silent.
    let wire_model = ledger.bodies[0]["model"].as_str().unwrap_or("<missing>");
    assert_eq!(
        wire_model, "Qwen3.5-27B.test",
        "non-streaming shared-primary PINS the resolved target on the wire \
         (the streaming sibling does not — see \
         a_shared_primary_reaches_the_peer_but_does_not_yet_pin_its_target). \
         If this is now empty, unifying the routing bodies silently dropped \
         the pin and the peer is routing on the envelope instead. Got {wire_model:?}"
    );
}
