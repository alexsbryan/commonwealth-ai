// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the chat_completion_e2e e2e suite — split from chat_completion_e2e.rs for the §3.2 size ceiling (behaviour-preserving move).

use sovereign_contracts::traits::InferenceProvider;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Query;
use axum::response::{sse::Event, IntoResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::ids::NodeId;
use oicp_types::{CapabilityHint, InferenceRequirements, LatencyClass};
use sovereign_contracts::types::{CompletionRequest, Speed};
use sovereign_daemon::daemon::InferenceVenue;

use super::{
    capabilities_handler, drain, local_byom, mip_with_peers, StreamQuery, PEER_RESPONSE_TEXT,
    STUB_NODE_ID,
};

// ── M5 piece 3: the identity stamp and the shed it arms ─────

/// `X-Node-Id` values the mock peer saw on `/v1/chat/completions`,
/// in arrival order. `None` = the header was absent on that request.
type NodeIdLog = Arc<std::sync::Mutex<Vec<Option<String>>>>;

/// Records the requester identity, then serves the ordinary stream.
///
/// The header is read on the CHAT route on purpose. `peer_inference`
/// has stamped it on the manifest fetch since long before M5, so a
/// test that watched `/oicp/v1/capabilities` would have passed
/// against the un-stamped build this commit fixes.
async fn node_id_capturing_chat_handler(
    axum::extract::State(log): axum::extract::State<NodeIdLog>,
    headers: axum::http::HeaderMap,
    Query(_q): Query<StreamQuery>,
) -> impl IntoResponse {
    log.lock().expect("node-id log poisoned").push(
        headers
            .get("x-node-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    );
    let delta = |s: &str| {
        serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": s}, "finish_reason": null}]
        })
        .to_string()
    };
    let (first, second) = PEER_RESPONSE_TEXT.split_at(PEER_RESPONSE_TEXT.len() / 2);
    Sse::new(futures::stream::iter(vec![
        Ok::<_, std::convert::Infallible>(Event::default().data(delta(first))),
        Ok(Event::default().data(delta(second))),
        Ok(Event::default().data("[DONE]")),
    ]))
    .into_response()
}

async fn spawn_node_id_capturing_peer() -> (SocketAddr, NodeIdLog) {
    let log: NodeIdLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities_handler))
        .route("/v1/chat/completions", post(node_id_capturing_chat_handler))
        .with_state(Arc::clone(&log));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, log)
}

/// A HEALTHY peer that declines to serve right now — byte-for-byte
/// the shape `commonwealth-api`'s admission layer emits when the
/// local user is at the keyboard (`AdmissionRejection`).
async fn shedding_chat_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::RETRY_AFTER, "34")],
        Json(serde_json::json!({
            "error": "peer is serving its own user",
            "reason": "yielded_to_local",
            "retry_after_secs": 34,
        })),
    )
}

/// A peer that is genuinely BROKEN. The control for the shed tests:
/// same failed turn, same code path, but a status that names a fault
/// rather than a refusal.
async fn faulting_chat_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "slot panicked",
    )
}

/// Serves a valid manifest — so the peer stays a live candidate and
/// is re-chosen on every turn — but answers chat with `status`.
async fn spawn_failing_peer(shedding: bool) -> SocketAddr {
    let chat = if shedding {
        post(shedding_chat_handler)
    } else {
        post(faulting_chat_handler)
    };
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities_handler))
        .route("/v1/chat/completions", chat);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// How many chat hops a peer actually received. The failed-hop tax the
/// §9.1.1 harness measures is exactly this count: every hop past the
/// first is a round-trip spent being told the same "no".
type HopCount = Arc<std::sync::atomic::AtomicUsize>;

/// A peer that yields to its local user and COUNTS the hops it refused.
/// Same body as `shedding_chat_handler` — the assertion is about how
/// many times we knocked, not what came back.
async fn counting_shedding_chat_handler(
    axum::extract::State(hops): axum::extract::State<HopCount>,
) -> impl IntoResponse {
    hops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::RETRY_AFTER, "34")],
        Json(serde_json::json!({
            "error": "peer is serving its own user",
            "reason": "yielded_to_local",
            "retry_after_secs": 34,
        })),
    )
}

/// Serves a valid manifest — so nothing but the yield backoff can take
/// this peer out of the candidate set — and counts refused chat hops.
async fn spawn_counting_yielding_peer() -> (SocketAddr, HopCount) {
    let hops: HopCount = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities_handler))
        .route(
            "/v1/chat/completions",
            post(counting_shedding_chat_handler).with_state(hops.clone()),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, hops)
}

// ═══════════════════════════════════════════════════════════════
// M5 piece 3 — the identity stamp, and the exemption that makes it
// safe to ship.
//
// These two belong together and are deliberately adjacent. Stamping
// `X-Node-Id` is what finally routes peer inference through the
// peer's admission gates; the shed exemption is what stops those
// gates' polite refusals from being booked as faults. Ship the first
// without the second and the mesh degrades under exactly the load
// M5 exists to survive.
// ═══════════════════════════════════════════════════════════════

/// One peer, pointed at `addr`, named "Founder".
fn founder_at(addr: SocketAddr) -> Vec<InferenceVenue> {
    vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![format!("http://{addr}/v1")],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }]
}

/// The DeepQuery-shaped, mesh-allowed request the other tests in this
/// file use to make routing choose the peer over the weak local BYOM.
fn mesh_allowed_request() -> CompletionRequest {
    CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed),
        )
}

#[tokio::test]
async fn a_peer_routed_turn_identifies_this_node_to_the_peer() {
    let (peer_addr, node_ids) = spawn_node_id_capturing_peer().await;
    let wrapper = mip_with_peers(local_byom(), founder_at(peer_addr));

    let (stream, attribution) = wrapper
        .complete_stream_with_id(&mesh_allowed_request())
        .await
        .expect("the peer must serve this turn");
    let text = drain(stream).await;
    assert_eq!(text, PEER_RESPONSE_TEXT);
    assert!(attribution.contains("@ peer Founder"), "{attribution:?}");

    // THE ASSERTION. Without the stamp the peer's admission layer
    // short-circuits on `is_peer == false` and this turn is admitted
    // as though the peer's own user had typed it — no pause, no
    // foreground yield, no `max_peer_inflight` ceiling. That is what
    // M5's 2026-08-06 experiment measured: four concurrent peer
    // requests, `peer_inflight_current` never leaving 0, the fourth
    // answering after 6.41 s with no signal.
    let seen = node_ids.lock().expect("node-id log poisoned");
    assert_eq!(seen.len(), 1, "exactly one chat request: {seen:?}");
    assert_eq!(
        seen[0].as_deref(),
        Some(NodeId::from_u128(STUB_NODE_ID).to_hex().as_str()),
        "the forwarded completion must carry this node's id as X-Node-Id — \
         it is the ONLY thing that distinguishes peer traffic from local \
         traffic at the receiving daemon (commonwealth-api/admission.rs)"
    );
}

/// A shed is a healthy peer saying "not right now". Booking it as a
/// fault quarantines the peer for 60 s after three of them — and with
/// `max_peer_inflight` defaulting to 1, three is what a handful of
/// concurrent turns produces.
#[tokio::test]
async fn repeated_sheds_never_quarantine_a_healthy_peer() {
    let peer_addr = spawn_failing_peer(true).await;
    let wrapper = mip_with_peers(local_byom(), founder_at(peer_addr));

    // Four — one past FAILURE_THRESHOLD, so a regression cannot pass
    // by arriving one short of the line. Each turn fails over to the
    // unconfigured local stub, which errors; the routing attempt is
    // what this test is about, not the answer.
    for _ in 0..4 {
        let _ = wrapper
            .complete_stream_with_id(&mesh_allowed_request())
            .await;
    }

    let health = wrapper.peer_health_snapshot();
    let founder = health.iter().find(|(name, ..)| name == "Founder");
    match founder {
        None => { /* never booked at all — the strongest possible pass */ }
        Some((_, quarantined, consecutive_failures, _)) => {
            assert!(
                !quarantined,
                "four sheds quarantined a healthy peer — it will now be dropped \
                 from the candidate set for a 60 s cooldown before its manifest \
                 is even read, which is a routing regression caused by M5's stamp"
            );
            assert_eq!(
                *consecutive_failures, 0,
                "a shed must not increment the consecutive-failure counter at all; \
                 counting-but-not-quarantining still poisons health_weight"
            );
        }
    }
}

/// §9.1.2's red, end to end: a peer that yields to its own local user
/// must be asked ONCE, not once per turn.
///
/// Measured at N=2 on 2026-08-14, before this landed: the scheduler
/// selected the peer on 421 of 672 dispatches and all 421 were refused
/// with `yielded_to_local` — a round-trip per turn to be told the same
/// thing (note 3234d770). The peer serves a valid manifest throughout,
/// so nothing but the yield backoff can take it out of the candidate
/// set, and it never becomes quarantined (a refusal is not a fault).
#[tokio::test]
async fn a_yielding_peer_is_asked_once_not_once_per_turn() {
    let (peer_addr, hops) = spawn_counting_yielding_peer().await;
    let wrapper = mip_with_peers(local_byom(), founder_at(peer_addr));

    for _ in 0..4 {
        let _ = wrapper
            .complete_stream_with_id(&mesh_allowed_request())
            .await;
    }

    let asked = hops.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        asked,
        1,
        "the peer said `yielded_to_local` with retry_after_secs=34 on the first \
         hop and was re-dialled {} more time(s) inside that window — this is the \
         failed-hop tax §9.1.1 measures",
        asked.saturating_sub(1)
    );

    // And it is backed off, not BROKEN: the health exemption for sheds
    // still holds, so the peer returns on its own when the window ends
    // rather than serving a quarantine cooldown.
    let health = wrapper.peer_health_snapshot();
    if let Some((_, quarantined, consecutive_failures, _)) =
        health.iter().find(|(name, ..)| name == "Founder")
    {
        assert!(!quarantined, "a yield refusal quarantined a healthy peer");
        assert_eq!(
            *consecutive_failures, 0,
            "a yield refusal was booked as a fault"
        );
    }
}

/// The control, and the reason the test above is a gate rather than a
/// tautology (ARCH §18.1): the same code path, the same failed turn,
/// a status that names a FAULT instead of a refusal — and the peer is
/// quarantined. If this ever goes green alongside a broken exemption,
/// the exemption has swallowed real failures too.
#[tokio::test]
async fn repeated_faults_still_quarantine_a_broken_peer() {
    let peer_addr = spawn_failing_peer(false).await;
    let wrapper = mip_with_peers(local_byom(), founder_at(peer_addr));

    for _ in 0..4 {
        let _ = wrapper
            .complete_stream_with_id(&mesh_allowed_request())
            .await;
    }

    let health = wrapper.peer_health_snapshot();
    let (_, quarantined, _, _) = health
        .iter()
        .find(|(name, ..)| name == "Founder")
        .expect("a peer that 500s four times must be booked against its health");
    assert!(
        quarantined,
        "a peer returning 500 four times is broken, not busy — it must still \
         quarantine, or the shed exemption has made peer health unfalsifiable"
    );
}
