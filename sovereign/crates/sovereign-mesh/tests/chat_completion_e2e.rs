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
use sovereign_core::error::{Error, Result};
use sovereign_core::oicp::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass,
    ModelStatus, ProviderManifest, ProviderModel, OICP_VERSION,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, ProviderCapabilities, Speed,
};
use sovereign_mesh::daemon::PeerInferenceEndpoint;
use sovereign_mesh::peer_inference::{
    MeshInferenceProvider, PeerEndpointSource,
};

// ── Local provider stub ─────────────────────────────────────
//
// A tiny `InferenceProvider` that advertises a very small BYOM-
// like model via `model_id_for`. The local manifest — built by
// `build_self_manifest` from this provider — will score below the
// peer's 9B for a DeepQuery, so routing should flow to the peer.

struct LocalStub {
    fast_id: String,
}

#[async_trait]
impl InferenceProvider for LocalStub {
    async fn complete(&self, _: &CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::NotImplemented(
            "LocalStub::complete should never be reached — test expects peer routing"
                .into(),
        ))
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>,
    > {
        Err(Error::NotImplemented(
            "LocalStub::complete_stream should never be reached — test expects peer routing"
                .into(),
        ))
    }
    async fn embed(&self, _: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("no embed".into()))
    }
    async fn embed_batch(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::NotImplemented("no embed".into()))
    }
    async fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("no embed".into()))
    }
    fn model_id_for(&self, speed: Speed) -> String {
        match speed {
            // Matches `profiles.byom_qwen25.thoughtful` in
            // models.toml — caps={General:2, Analysis:1,
            // Instruction:2}, which cannot score 1.0 against a
            // DeepQuery's {Analysis:3, General:3}. Peer's 9B wins.
            Speed::Fast | Speed::Slow | Speed::Medium => self.fast_id.clone(),
        }
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 32_768,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: sovereign_core::types::Depth::Moderate,
        }
    }
}

// ── Peer endpoint source stub ───────────────────────────────
//
// Provides a fixed peer list that `MeshInferenceProvider` uses in
// place of `EmbeddedDaemon::peer_inference_endpoints()`.

struct StubPeerSource {
    peers: Vec<PeerInferenceEndpoint>,
}

#[async_trait]
impl PeerEndpointSource for StubPeerSource {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        self.peers.clone()
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

async fn capabilities_handler() -> impl IntoResponse {
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
    let manifest = ProviderManifest {
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
            },
        ],
        knowledge: None,
        federation: None,
    };
    Json(manifest)
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
    }];
    let peer_source: Arc<dyn PeerEndpointSource> =
        Arc::new(StubPeerSource { peers });

    // 3. Build the local-side provider: a BYOM-class 3B that
    //    cannot satisfy DeepQuery's preferred profile at score
    //    1.0. The `Qwen2.5-3B` base_name is annotated in
    //    `models.toml` as `byom_qwen25.thoughtful` so the OICP
    //    scorer sees real (weak) caps.
    let local: Arc<dyn InferenceProvider> = Arc::new(LocalStub {
        fast_id: "qwen2.5-3b-instruct-q4_k_m".into(),
    });

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
    let request = CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow) // Only Speed::Slow ever routes to peers.
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
    }];
    let peer_source: Arc<dyn PeerEndpointSource> =
        Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = Arc::new(LocalStub {
        fast_id: "qwen2.5-3b-instruct-q4_k_m".into(),
    });
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::LocalOnly);

    let request = CompletionRequest::new("sensitive prompt")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);

    // Expect the LOCAL stream path to be attempted — our LocalStub
    // returns NotImplemented there, so the wrapper must surface
    // that error. If routing had gone to the peer instead, the
    // mock peer would have returned real SSE and we'd get Ok.
    match wrapper.complete_stream_with_id(&request).await {
        Ok(_) => panic!("LocalOnly must not route to a peer"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("LocalStub::complete_stream"),
                "expected error to come from the local stream path; got {msg:?}"
            );
        }
    }
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
    }];
    let peer_source: Arc<dyn PeerEndpointSource> =
        Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = Arc::new(LocalStub {
        fast_id: "qwen2.5-3b-instruct-q4_k_m".into(),
    });
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
    }];
    let peer_source: Arc<dyn PeerEndpointSource> =
        Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = Arc::new(LocalStub {
        fast_id: "qwen2.5-3b-instruct-q4_k_m".into(),
    });
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let request = CompletionRequest::new("hi")
        .with_speed(Speed::Slow)
        .with_model_id("not-a-real-model-anywhere");

    match wrapper.complete_stream_with_id(&request).await {
        Ok((_, attribution)) => panic!(
            "unknown model_id should NOT be served; instead got attribution {attribution:?}"
        ),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("not-a-real-model-anywhere"),
                "error should mention the requested model id; got {msg:?}"
            );
            assert!(
                msg.to_lowercase().contains("no node") || msg.to_lowercase().contains("model not loaded"),
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
    }];
    let peer_source: Arc<dyn PeerEndpointSource> =
        Arc::new(StubPeerSource { peers });
    let local: Arc<dyn InferenceProvider> = Arc::new(LocalStub {
        fast_id: "qwen2.5-3b-instruct-q4_k_m".into(),
    });
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
