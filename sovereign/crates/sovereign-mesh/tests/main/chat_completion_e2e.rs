// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end mesh inference routing test.
//!
//! Exercises the full Joiner-side path without needing a real
//! `EmbeddedDaemon`:
//!   1. `MeshInferenceProvider::complete_stream_with_id` selects a
//!      peer based on OICP scoring + the 60s manifest cache.
//!   2. It calls `GET /oicp/v1/capabilities` on the peer to fetch
//!      the manifest.
//!   3. It compares the peer's best candidate against the local
//!      manifest and decides to route.
//!   4. It POSTs `/v1/chat/completions?stream=true` to the peer
//!      and returns the stream + an attribution string.
//!
//! A `MockPeerServer` plays the Founder side — a minimal axum
//! router that serves a curated OICP manifest at the capabilities
//! endpoint and a canned SSE stream at the chat endpoint. No real
//! llama.cpp or EmbeddedDaemon is involved.
//!
//! Guards the two bugs we fixed in this body of work:
//!   * Multi-slot manifest advertisement (peer must see both Fast
//!     and Slow slots; the 9B picks up the request even though a
//!     27B is also advertised).
//!   * Streaming provenance attribution (the returned `model_id`
//!     string must carry `@ peer <name>`).
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::Query;
use axum::response::{sse::Event, IntoResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::ids::NodeId;
use futures::StreamExt;
use serde::Deserialize;
use sovereign_core::oicp::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus,
    ProviderManifest, ProviderModel, OICP_VERSION,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_mesh::daemon::PeerInferenceEndpoint;
use sovereign_mesh::peer_inference::{MeshInferenceProvider, PeerEndpointSource};

use crate::common;
use crate::common::TestProvider;

// Local provider — a weak BYOM-like model whose `model_id_for`
// resolves to `profiles.byom_qwen25.thoughtful` in `models.toml`
// (caps={General:2, Analysis:1, Instruction:2}). It cannot score
// 1.0 against a DeepQuery's {Analysis:3, General:3}, so the
// peer's 9B wins and routing flows to the peer. The local stub
// is left in the unconfigured state — any local-path attempt
// surfaces `TestProvider::*_not_configured` as the bubbled error,
// which is what the LocalOnly test asserts on.
fn local_byom() -> Arc<dyn InferenceProvider> {
    Arc::new(TestProvider::new().with_model_id("qwen2.5-3b-instruct-q4_k_m"))
}

// ── Peer endpoint source stub ───────────────────────────────
//
// Provides a fixed peer list that `MeshInferenceProvider` uses in
// place of `EmbeddedDaemon::peer_inference_endpoints()`.

struct StubPeerSource {
    peers: Vec<PeerInferenceEndpoint>,
}

/// The id this stub claims as its own. A real `EmbeddedDaemon`
/// answers `local_node_id` from its joined mesh identity; the stub
/// answers with this so the routing path under test stamps
/// `X-Node-Id` exactly as production does.
const STUB_NODE_ID: u128 = 0x00C0_FFEE;

#[async_trait]
impl PeerEndpointSource for StubPeerSource {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        self.peers.clone()
    }

    /// Overridden deliberately. The trait's default is `None`, and a
    /// `None` here would make every routing test in this file forward
    /// UNSTAMPED — i.e. would keep asserting the pre-M5 behaviour
    /// while production stamps. The absence case is covered where its
    /// decider lives, at the wire, in `oicp-client`'s own tests.
    async fn local_node_id(&self) -> Option<NodeId> {
        Some(NodeId::from_u128(STUB_NODE_ID))
    }
}

// ── Mock peer HTTP server (the "Founder" role) ──────────────
//
// Serves:
//   * GET /oicp/v1/capabilities  → JSON ProviderManifest (9B + 27B)
//   * POST /v1/chat/completions  → SSE stream of canned deltas
// Nothing else. This is the minimum surface `MeshInferenceProvider`
// consults when routing a single streaming completion.

const PEER_RESPONSE_TEXT: &str = "Hello from Founder's 9B slot.";

#[derive(Deserialize)]
struct StreamQuery {
    #[serde(default)]
    stream: Option<bool>,
}

fn two_slot_manifest(features: Vec<String>) -> ProviderManifest {
    // Two slots, same shape as `build_self_manifest` produces on a
    // real high-profile Founder after the multi-slot change:
    // Qwen3.5-9B (5.5 GB, score ~0.75 on DeepQuery) and
    // Qwen3.5-27B (16.5 GB, score ~0.85 on DeepQuery). Since both
    // are general-hint at Normal latency, the request's Extended
    // latency hits both at the adjacent-class bonus and the 27B's
    // higher affinity wins — unless the pick_better tie-break
    // (smaller size_gb) promotes the 9B. The v0.3 wire tests below
    // use a different scoring assumption; the key assertion is that
    // routing reaches the peer at all, not which slot it lands on.
    ProviderManifest {
        oicp_version: OICP_VERSION.into(),
        provider: None,
        models: vec![
            ProviderModel {
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
                claims: vec![CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Normal,
                    32_768,
                    4_000,
                    0.80,
                )],
                fingerprint: None,
            },
            ProviderModel {
                id: "Qwen3.5-27B.test".into(),
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
                size_gb: Some(16.5),
                // Equal affinity to the 9B so the score_manifest_for_request
                // pick_better tiebreaker falls to the smaller size_gb
                // (5.5 < 16.5 → Qwen3.5-9B wins). Mirrors the original
                // v0.2 assumption that DeepQuery scores 1.0 on both
                // slots.
                claims: vec![CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Normal,
                    32_768,
                    4_000,
                    0.80,
                )],
                fingerprint: None,
            },
        ],
        knowledge: None,
        federation: None,
        features,
    }
}

async fn capabilities_handler() -> impl IntoResponse {
    Json(two_slot_manifest(Vec::new()))
}

/// Capabilities of a peer that DOES advertise the forced-choice feature.
async fn capabilities_handler_fc() -> impl IntoResponse {
    Json(two_slot_manifest(vec![
        sovereign_core::oicp::features::X_FORCED_CHOICE.to_string(),
    ]))
}

async fn chat_completions_handler(
    Query(q): Query<StreamQuery>,
    Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // The Joiner always requests streaming (`stream: true`), but
    // guard against drift — if the client asked for non-streaming
    // the test should fail loudly rather than hang.
    let _ = q.stream; // Stream flag is also in body; don't enforce here.

    // Emit two OpenAI-style SSE deltas plus the [DONE] sentinel.
    // `RemoteApiProvider::complete_stream` joins the deltas into
    // one text string; two chunks instead of one proves we're not
    // accidentally dropping fragments.
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
    let stream = futures::stream::iter(events);
    Sse::new(stream).into_response()
}

/// Bodies the mock peer actually received, in arrival order.
type BodyLog = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

/// Same SSE response as `chat_completions_handler`, but records the
/// request body first.
///
/// The plain mock accepts any body and never resolves the `model`
/// field, which is precisely why it cannot catch a dispatch that
/// names a model the receiving node does not advertise — it proves
/// the transport works, not that the payload is serviceable. This
/// variant exists so a test can assert on what actually goes on the
/// wire.
async fn capturing_chat_handler(
    axum::extract::State(log): axum::extract::State<BodyLog>,
    Query(_q): Query<StreamQuery>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    log.lock().expect("body log poisoned").push(body);
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

async fn spawn_capturing_peer() -> (SocketAddr, BodyLog) {
    let log: BodyLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities_handler))
        .route("/v1/chat/completions", post(capturing_chat_handler))
        .with_state(Arc::clone(&log));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, log)
}

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

async fn spawn_mock_peer() -> SocketAddr {
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities_handler))
        .route("/v1/chat/completions", post(chat_completions_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Tokio needs a moment to start accepting; the wrapper's
    // 800ms manifest timeout is comfortably above this but the
    // test is flakier if we don't.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// Like `spawn_mock_peer` but the capabilities endpoint advertises the
/// `x:forced_choice` feature — used by the forced-choice scheduler-filter
/// tests to distinguish an eligible peer from an excluded one.
async fn spawn_mock_peer_fc() -> SocketAddr {
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities_handler_fc))
        .route("/v1/chat/completions", post(chat_completions_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

// ── The test ────────────────────────────────────────────────

#[tokio::test]
async fn joiner_streams_through_mesh_and_attributes_peer() {
    // 1. Stand up the mock peer.
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    // 2. Build the stub peer source — one peer, the Founder.
    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });

    // 3. Build the local-side provider: a BYOM-class 3B that
    //    cannot satisfy DeepQuery's preferred profile at score
    //    1.0. The `Qwen2.5-3B` base_name is annotated in
    //    `models.toml` as `byom_qwen25.thoughtful` so the OICP
    //    scorer sees real (weak) caps.
    let local: Arc<dyn InferenceProvider> = local_byom();

    // 4. The wrapper under test.
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    // 5. Build a DeepQuery-shaped request — this is what
    //    `runtime::build_oicp` emits for Intent::DeepQuery.
    // Spec default for `ShardingPrivacy` is `LocalOnly`, so the
    // envelope must explicitly opt into mesh routing. In production
    // `runtime::build_oicp` does this automatically via skill
    // configuration; the test reproduces the DeepQuery defaults
    // that `sovereign-core` emits at runtime.
    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    // The OICP envelope (MeshAllowed + Extended latency) is what
    // makes this offload-eligible per SLOT_POLICY §5. The Speed
    // literal is a derived shadow and no longer gates routing — see
    // `mesh_allowed_normal_latency_routes_to_peer_without_speed_signal`
    // below, which routes to a peer on a Fast-speed request.
    let request = CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);

    // 6. Exercise the full path.
    let (mut stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("peer route should succeed");

    // 7. Attribution: the returned model_id must carry
    //    `@ peer Founder` AND name the 9B slot (the tiebreaker
    //    pick), not the 27B. This is the flagship assertion
    //    guarding both fixes in this body of work.
    assert!(
        model_id.contains("@ peer Founder"),
        "model_id should carry peer attribution; got {model_id:?}"
    );
    assert!(
        model_id.contains("9B"),
        "model_id should name the 9B slot (smaller wins the OICP tie-break); got {model_id:?}"
    );
    assert!(
        !model_id.contains("27B"),
        "model_id must NOT name the 27B slot (the tie-break loser); got {model_id:?}"
    );

    // 8. Drain the stream and confirm we got the canned body.
    let mut collected = String::new();
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.expect("stream chunk should be Ok"));
    }
    assert_eq!(collected, PEER_RESPONSE_TEXT);
}

/// Defect 1: structured-503 → alternate-peer failover. The best peer 503s on
/// dispatch; the ranked cascade must fail over to the next-best peer instead of
/// collapsing straight to local (which errors here). Pre-fix (single-peer
/// `select_peer`) this routed Busy → LocalFallback → local error.
#[tokio::test]
async fn oicp_503_fails_over_to_next_peer() {
    async fn caps_strong() -> impl IntoResponse {
        // Higher affinity than the plain peer (0.80) so this peer ranks FIRST.
        let manifest = ProviderManifest {
            oicp_version: OICP_VERSION.into(),
            provider: None,
            models: vec![ProviderModel {
                id: "Qwen3.5-9B.strong".into(),
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
                    0.92,
                )],
                fingerprint: None,
            }],
            knowledge: None,
            federation: None,
            features: Vec::new(),
        };
        Json(manifest)
    }
    async fn chat_503() -> impl IntoResponse {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "busy",
                "reason": "ceiling_exceeded",
                "retry_after_secs": 2
            })),
        )
    }

    // Busy peer: ranks first (0.92) but 503s on chat dispatch.
    let busy_addr = {
        let app = Router::new()
            .route("/oicp/v1/capabilities", get(caps_strong))
            .route("/v1/chat/completions", post(chat_503));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        addr
    };
    // Good peer: plain manifest (0.80) + real SSE.
    let good_addr = spawn_mock_peer().await;

    let peers = vec![
        PeerInferenceEndpoint {
            node_id: NodeId::from_u128(1),
            name: "Busy".into(),
            base_urls: vec![format!("http://{busy_addr}/v1")],
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
            inference_availability: None,
            gossip_last_seen_unix: 0,
            transport: None,
        },
        PeerInferenceEndpoint {
            node_id: NodeId::from_u128(2),
            name: "Good".into(),
            base_urls: vec![format!("http://{good_addr}/v1")],
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
            inference_availability: None,
            gossip_last_seen_unix: 0,
            transport: None,
        },
    ];
    let wrapper =
        MeshInferenceProvider::with_peer_source(local_byom(), Arc::new(StubPeerSource { peers }));
    let request = CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed),
        );

    let (mut stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("the 503 from the best peer should fail over to the next peer");

    assert!(
        model_id.contains("@ peer Good"),
        "must fail over to the Good peer; got {model_id:?}"
    );
    assert!(
        !model_id.contains("Busy"),
        "must not attribute the 503 peer; got {model_id:?}"
    );
    let mut collected = String::new();
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.expect("stream chunk should be Ok"));
    }
    assert_eq!(
        collected, PEER_RESPONSE_TEXT,
        "should stream the Good peer's body"
    );
}

/// Defect 2: mid-stream peer death must NOT duplicate tokens. A peer returns
/// 200 + one delta, then the stream ends abruptly (no `[DONE]`). The consumer
/// must see that partial text exactly ONCE — no local re-run, no duplication.
/// Pins the no-double-emit invariant the ranked-failover cascade preserves.
#[tokio::test]
async fn peer_dies_mid_stream_does_not_duplicate() {
    async fn chat_truncated(
        Query(_q): Query<StreamQuery>,
        Json(_b): Json<serde_json::Value>,
    ) -> impl IntoResponse {
        let delta = |s: &str| {
            serde_json::json!({
                "choices": [{ "index": 0, "delta": { "content": s }, "finish_reason": null }]
            })
            .to_string()
        };
        // ONE delta, then the stream ends — no second delta, no [DONE].
        let events = vec![Ok::<_, std::convert::Infallible>(
            Event::default().data(delta("partial-")),
        )];
        Sse::new(futures::stream::iter(events)).into_response()
    }

    let peer_addr = {
        let app = Router::new()
            .route("/oicp/v1/capabilities", get(capabilities_handler))
            .route("/v1/chat/completions", post(chat_truncated));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        addr
    };
    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(7),
        name: "Truncator".into(),
        base_urls: vec![format!("http://{peer_addr}/v1")],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let wrapper =
        MeshInferenceProvider::with_peer_source(local_byom(), Arc::new(StubPeerSource { peers }));
    let request = CompletionRequest::new("Q")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed),
        );

    let (mut stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("peer stream should start (200)");
    assert!(model_id.contains("@ peer Truncator"), "got {model_id:?}");

    let mut collected = String::new();
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.unwrap_or_default());
    }
    // Exactly the one partial delta — no duplication, no local re-run appended.
    assert_eq!(
        collected, "partial-",
        "mid-stream death must yield the partial token ONCE, not duplicated"
    );
}

/// Mirror of the above but with `ShardingPrivacy::LocalOnly` — the
/// `inner-work`-class skills set this flag to forbid crossing the
/// network. Wrapper must fall back to `local.complete_stream`,
/// which our stub errors on, so we assert the request surfaces
/// the local-path error rather than routing to the peer.
#[tokio::test]
async fn local_only_sharding_never_routes_to_peer() {
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::LocalOnly);

    let request = CompletionRequest::new("sensitive prompt")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);

    // Expect the LOCAL stream path to be attempted — our local
    // provider is the unconfigured `TestProvider` which surfaces
    // `NotImplemented` from `complete_stream`. If routing had
    // gone to the peer instead, the mock peer would have
    // returned real SSE and we'd get Ok.
    match wrapper.complete_stream_with_id(&request).await {
        Ok(_) => panic!("LocalOnly must not route to a peer"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("complete_stream"),
                "expected error to come from the local stream path; got {msg:?}"
            );
        }
    }
}

/// SLOT_POLICY §5 headline — the OICP envelope decides offload, not
/// the `Speed` shadow. A `MeshAllowed` + `Normal`-latency request on
/// a **`Speed::Fast`** turn (no `Speed::Slow` signal at all) must
/// route to the peer. Under the old gate the `preferred_speed != Slow`
/// check bailed this to local before the envelope was ever consulted;
/// now `offload_eligible` (MeshAllowed && latency != Fast) admits it.
#[tokio::test]
async fn mesh_allowed_normal_latency_routes_to_peer_without_speed_signal() {
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    // Normal latency is an EXACT class match for the mock peer's
    // Normal-latency claims, so the peer scores at least as well as
    // in `joiner_streams_through_mesh_and_attributes_peer` (which
    // uses adjacent-class Extended and still routes).
    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let request = CompletionRequest::new("summarize this thread")
        .with_speed(Speed::Fast) // deliberately NOT Slow — the envelope decides.
        .with_oicp(envelope);

    let (mut stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("MeshAllowed + non-Fast latency must route to the peer regardless of Speed");
    assert!(
        model_id.contains("@ peer Founder"),
        "envelope-eligible request should route to peer; got {model_id:?}"
    );

    let mut collected = String::new();
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.expect("stream chunk should be Ok"));
    }
    assert_eq!(collected, PEER_RESPONSE_TEXT);
}

/// SLOT_POLICY §5 privacy gate — a `LocalOnly` request stays local
/// no matter its latency class. This is the judge-shaped case: a
/// Normal-latency grounding judge on a private turn must never cross
/// the network. Distinct from `local_only_sharding_never_routes_to_peer`
/// (which uses Extended latency) — proves the privacy gate is
/// latency-independent.
#[tokio::test]
async fn local_only_judge_shaped_request_stays_local() {
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::LocalOnly);
    let request = CompletionRequest::new("grade this answer against the evidence")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);

    match wrapper.complete_stream_with_id(&request).await {
        Ok(_) => panic!("LocalOnly must not route to a peer, even at Normal latency"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("complete_stream"),
                "expected the local stream path error; got {msg:?}"
            );
        }
    }
}

/// SLOT_POLICY §5 latency gate — latency-`Fast` work never offloads,
/// even on a `MeshAllowed` mesh and even carrying a `Speed::Slow`
/// literal. The round-trip dominates the inference for router/title/
/// compression-class turns, so `offload_eligible` fails them closed.
#[tokio::test]
async fn latency_fast_never_routes_even_when_mesh_allowed() {
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Fast)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let request = CompletionRequest::new("route: is this a question or a command?")
        .with_speed(Speed::Slow) // even a Slow shadow cannot override latency Fast.
        .with_oicp(envelope);

    match wrapper.complete_stream_with_id(&request).await {
        Ok(_) => panic!("latency Fast must stay local even when MeshAllowed"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("complete_stream"),
                "expected the local stream path error; got {msg:?}"
            );
        }
    }
}

/// SLOT_POLICY §6 — a forced-choice sentinel must NOT route to a peer
/// whose manifest lacks `x:forced_choice`: that peer would silently fall
/// back to K-sampling, defeating the one-pass calibrated elicitation. The
/// default mock peer advertises no features, so the scheduler filter
/// excludes it and the request stays local (our stub errors there).
#[tokio::test]
async fn forced_choice_sentinel_excludes_peer_without_feature() {
    let peer_addr = spawn_mock_peer().await; // advertises NO features
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let mut request = CompletionRequest::new("pick one: A or B")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);
    request.max_tokens = Some(1);
    request.structured_output = Some(serde_json::json!({
        "type": "string",
        "enum": ["A", "B"],
        "x_forced_choice": true,
    }));

    match wrapper.complete_stream_with_id(&request).await {
        Ok(_) => panic!("forced-choice sentinel must not route to a peer lacking x:forced_choice"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("complete_stream"),
                "expected the local stream path error; got {msg:?}"
            );
        }
    }
}

/// SLOT_POLICY §6 — the same sentinel DOES route to a peer that
/// advertises `x:forced_choice`. Confirms the filter excludes on absence,
/// not on the sentinel's presence.
#[tokio::test]
async fn forced_choice_sentinel_routes_to_peer_advertising_feature() {
    let peer_addr = spawn_mock_peer_fc().await; // advertises x:forced_choice
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let mut request = CompletionRequest::new("pick one: A or B")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);
    request.max_tokens = Some(1);
    request.structured_output = Some(serde_json::json!({
        "type": "string",
        "enum": ["A", "B"],
        "x_forced_choice": true,
    }));

    let (mut stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("feature-advertising peer must receive the forced-choice sentinel");
    assert!(
        model_id.contains("@ peer Founder"),
        "should route to the feature-advertising peer; got {model_id:?}"
    );

    let mut collected = String::new();
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.expect("stream chunk should be Ok"));
    }
    assert_eq!(collected, PEER_RESPONSE_TEXT);
}

// ── Explicit `model` field routing ─────────────────────────────
//
// Guards the silent-substitution bug: a `model: "<peer-only-id>"`
// request without an OICP envelope must not be answered by the
// local primary slot. Three scenarios — peer-only model, unknown
// model, peer-only model on a Fast-speed request that today's
// OICP path would have refused to consider for peer routing.

/// A request with `model_id = "Qwen3.5-9B.test"` (advertised by
/// the mock peer, NOT by the local stub) must route to the peer
/// even with no OICP envelope and no Speed::Slow signal.
#[tokio::test]
async fn explicit_peer_model_id_routes_to_peer_without_oicp_envelope() {
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    // No OICP envelope, Speed::Fast (which would normally bail
    // peer routing). The model name is the routing signal.
    let request = CompletionRequest::new("hi")
        .with_speed(Speed::Fast)
        .with_model_id("Qwen3.5-9B.test");

    let (mut stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("explicit peer model_id should route to peer");
    assert!(
        model_id.contains("Qwen3.5-9B.test"),
        "attribution should name the requested model; got {model_id:?}"
    );
    assert!(
        model_id.contains("@ peer Founder"),
        "attribution should carry peer suffix; got {model_id:?}"
    );

    let mut collected = String::new();
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.expect("stream chunk should be Ok"));
    }
    assert_eq!(collected, PEER_RESPONSE_TEXT);
}

/// A `model` name that no node advertises must surface as a clear
/// error rather than be silently substituted with the local primary.
/// This is the bug we are explicitly closing.
#[tokio::test]
async fn explicit_unknown_model_id_errors_instead_of_silent_substitution() {
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let request = CompletionRequest::new("hi")
        .with_speed(Speed::Slow)
        .with_model_id("not-a-real-model-anywhere");

    match wrapper.complete_stream_with_id(&request).await {
        Ok((_, attribution)) => {
            panic!("unknown model_id should NOT be served; instead got attribution {attribution:?}")
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("not-a-real-model-anywhere"),
                "error should mention the requested model id; got {msg:?}"
            );
            assert!(
                msg.to_lowercase().contains("no node")
                    || msg.to_lowercase().contains("model not loaded"),
                "error should signal that no node advertises the model; got {msg:?}"
            );
        }
    }
}

/// Empty/whitespace `model_id` is not a routing signal — the
/// request should fall through to the OICP-driven path. This pins
/// the back-compat contract for callers that pass `model: ""`.
#[tokio::test]
async fn empty_model_id_falls_through_to_oicp_path() {
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let request = CompletionRequest::new("hi")
        .with_speed(Speed::Slow)
        .with_oicp(envelope)
        .with_model_id("   "); // whitespace-only, treat as None

    // Should reach the OICP-driven peer route (mock peer beats
    // the BYOM local stub on score+size). Same outcome as
    // `joiner_streams_through_mesh_and_attributes_peer`.
    let (_stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("OICP-driven peer route should succeed");
    assert!(
        model_id.contains("@ peer Founder"),
        "OICP route should still attribute to peer; got {model_id:?}"
    );
}

/// An unnamed ranked dispatch must put a model field on the wire that
/// the receiving peer can actually resolve.
///
/// `explicit_model_id` (`peer_inference.rs:2028`) and `build_request`
/// (`oicp-client/src/lib.rs:239`) disagree about what "unnamed" means.
/// The former trims and rejects empty, so `None`, `Some("")` and
/// `Some("  ")` all fall through to the ranked path. The latter matches
/// only on `is_none()`, and maps that case to the peer provider's own
/// `model_id` — which `provider_for_peer` (`peer_inference.rs:2053`)
/// hardcodes to the placeholder `"mesh-peer"`. Nobody advertises that
/// name, so the receiving node's named path returns `ModelNotLoaded`
/// and 503s (confirmed against a live daemon, 2026-07-27).
///
/// `None` is not a corner case: `build_completion_request`
/// (`inference_adapter.rs:324-329`) normalises empty/whitespace to
/// `None`, so it is the ONLY shape the HTTP path can produce for a
/// request with no model pinned.
///
/// Latency matters because `latency_to_speed(Normal|Extended)` is
/// `Speed::Slow` (`slot_policy.rs:196-201`), which is the arm that
/// substitutes the placeholder. Fast-class requests send `""` and are
/// unaffected — this is why fast-lane offload works and knowledge
/// turns do not.
///
/// The sibling tests here cannot catch this: `chat_completions_handler`
/// accepts any body and never resolves `model`, so it proves the
/// transport works rather than that the payload is serviceable.
#[tokio::test]
async fn an_unnamed_ranked_dispatch_sends_a_model_the_peer_can_resolve() {
    let (peer_addr, bodies) = spawn_capturing_peer().await;
    let peers = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![format!("http://{}/v1", peer_addr)],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }];
    let wrapper =
        MeshInferenceProvider::with_peer_source(local_byom(), Arc::new(StubPeerSource { peers }));

    // Normal latency + MeshAllowed, model_id LEFT UNSET — the exact
    // shape `build_completion_request` produces for an inbound chat
    // that pins no model.
    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let request = CompletionRequest::new("Summarise the argument for compatibilism.")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);

    let (stream, _model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("ranked route should reach the peer");
    // Drain so the dispatch completes before we read the log.
    let _: Vec<_> = stream.collect().await;

    let body = bodies
        .lock()
        .expect("body log poisoned")
        .first()
        .cloned()
        .expect("setup sanity: the peer was never dispatched to at all");

    let model = body["model"].as_str().unwrap_or("<missing>");
    assert_ne!(
        model, "mesh-peer",
        "the ranked dispatch put the peer provider's placeholder id on the wire. \
         No node advertises 'mesh-peer', so the receiving peer's named path returns \
         ModelNotLoaded and 503s; the origin then records a peer failure and falls \
         back to local, quarantining a healthy peer after three strikes. Every \
         unnamed Normal/Extended offload is affected. Full body: {body}"
    );
    assert!(
        model.trim().is_empty(),
        "an unnamed ranked dispatch must stay unnamed on the wire so the peer routes \
         on the OICP envelope, but it carried model={model:?}. Full body: {body}"
    );
    assert!(
        body.get("oicp").is_some(),
        "dropping the envelope leaves the peer with neither a resolvable name nor a \
         routing opinion. Full body: {body}"
    );
}

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
struct PeerLedger {
    bodies: Vec<serde_json::Value>,
    /// Requests this peer actually generated tokens for.
    served: usize,
    /// Requests refused because `model` named something it does not
    /// advertise — the outage signature.
    refused_unresolvable: usize,
}

type PeerLedgerHandle = Arc<std::sync::Mutex<PeerLedger>>;

struct ResolvingPeerState {
    manifest: ProviderManifest,
    advertised: Vec<String>,
    ledger: PeerLedgerHandle,
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
    axum::extract::State(state): axum::extract::State<Arc<ResolvingPeerState>>,
) -> impl IntoResponse {
    Json(state.manifest.clone())
}

async fn resolving_chat_handler(
    axum::extract::State(state): axum::extract::State<Arc<ResolvingPeerState>>,
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

async fn spawn_resolving_peer() -> (SocketAddr, PeerLedgerHandle) {
    let manifest = aliased_manifest();
    let advertised: Vec<String> = manifest.models.iter().map(|m| m.id.clone()).collect();
    let ledger: PeerLedgerHandle = Arc::new(std::sync::Mutex::new(PeerLedger::default()));
    let state = Arc::new(ResolvingPeerState {
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

fn founder_endpoint(addr: SocketAddr) -> PeerInferenceEndpoint {
    PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![format!("http://{}/v1", addr)],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
    }
}

fn mesh_allowed_envelope() -> InferenceRequirements {
    InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed)
}

fn provider_with_resolving_peer(addr: SocketAddr) -> MeshInferenceProvider {
    MeshInferenceProvider::with_peer_source(
        local_byom(),
        Arc::new(StubPeerSource {
            peers: vec![founder_endpoint(addr)],
        }),
    )
}

async fn drain(
    stream: impl futures::Stream<Item = Result<String, sovereign_core::error::Error>>,
) -> String {
    let mut collected = String::new();
    let mut stream = Box::pin(stream);
    while let Some(chunk) = stream.next().await {
        collected.push_str(&chunk.expect("stream chunk should be Ok"));
    }
    collected
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
fn founder_at(addr: SocketAddr) -> Vec<PeerInferenceEndpoint> {
    vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![format!("http://{addr}/v1")],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        transport: None,
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
                .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed),
        )
}

#[tokio::test]
async fn a_peer_routed_turn_identifies_this_node_to_the_peer() {
    let (peer_addr, node_ids) = spawn_node_id_capturing_peer().await;
    let wrapper = MeshInferenceProvider::with_peer_source(
        local_byom(),
        Arc::new(StubPeerSource {
            peers: founder_at(peer_addr),
        }),
    );

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
    let wrapper = MeshInferenceProvider::with_peer_source(
        local_byom(),
        Arc::new(StubPeerSource {
            peers: founder_at(peer_addr),
        }),
    );

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
    let wrapper = MeshInferenceProvider::with_peer_source(
        local_byom(),
        Arc::new(StubPeerSource {
            peers: founder_at(peer_addr),
        }),
    );

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
    let wrapper = MeshInferenceProvider::with_peer_source(
        local_byom(),
        Arc::new(StubPeerSource {
            peers: founder_at(peer_addr),
        }),
    );

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
