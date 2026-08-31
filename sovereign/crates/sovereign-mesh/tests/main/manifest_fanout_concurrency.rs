// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-peer manifest fetch: concurrent, and single-flighted.
//!
//! Two independent defects, one loop (order `mesh-scale-t0` item 3,
//! `MESH_SCALE_100_USERS_1000_CORPORA.md` §7.4):
//!
//! 1. **Serial.** `select_peer`'s ranking loop and
//!    `gather_peer_candidates` both awaited each peer's manifest in
//!    turn, so a P-peer mesh put up to `P × MANIFEST_FETCH_TIMEOUT`
//!    (800 ms) in FRONT of the first token. The peers do not depend on
//!    each other; only the waiting was serialised.
//! 2. **No in-flight dedup.** `peer_cache` deduplicates across TIME
//!    (a hit inside `MANIFEST_TTL` is free) but not across CONCURRENCY:
//!    when the TTL lapses, every concurrent caller misses at once and
//!    each opens its own round-trip to the same peer. Worst exactly
//!    when the peer is slow, because a slow peer is what keeps callers
//!    overlapping.
//!
//! Both tests measure the REAL provider against REAL HTTP peers that
//! answer slowly on purpose.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::ids::NodeId;
use sovereign_core::oicp::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus,
    ProviderManifest, ProviderModel, ShardingPrivacy, OICP_VERSION,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_mesh::daemon::PeerInferenceEndpoint;
use sovereign_mesh::peer_inference::{MeshInferenceProvider, PeerEndpointSource};

use crate::common;
use crate::common::TestProvider;

// ─── harness ────────────────────────────────────────────────────

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

/// How long each mock peer takes to answer `/oicp/v1/capabilities`.
/// Comfortably inside `MANIFEST_FETCH_TIMEOUT` (800 ms) so nothing
/// times out — the test is about overlap, not about failure.
const MANIFEST_DELAY: Duration = Duration::from_millis(400);

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

async fn chat_completions_handler() -> axum::response::Response {
    Json(serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "Qwen3.5-9B.test",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": PEER_TEXT },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12 }
    }))
    .into_response()
}

/// A peer whose manifest endpoint is deliberately slow, and which
/// COUNTS how many times it was asked. The counter is what makes the
/// single-flight claim measurable rather than asserted.
async fn spawn_slow_peer(hits: Arc<AtomicUsize>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/oicp/v1/capabilities",
            get(move || {
                let hits = Arc::clone(&hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(MANIFEST_DELAY).await;
                    Json(peer_manifest())
                }
            }),
        )
        .route("/v1/chat/completions", post(chat_completions_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// DISTINCT `node_id` per peer — `peer_cache` and the single-flight
/// gate are both keyed by it, so reusing one id would make four peers
/// share one cache entry and the test would measure nothing.
fn peer_endpoint(idx: u128, addr: SocketAddr) -> PeerInferenceEndpoint {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    PeerInferenceEndpoint {
        node_id: NodeId::from_u128((0x42 + idx) << 100),
        name: format!("peer-{idx}"),
        base_urls: vec![format!("http://{addr}/v1")],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: Some(0),
        inference_availability: Some(0.95),
        gossip_last_seen_unix: now,
        transport: None,
    }
}

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

fn build(peers: Vec<PeerInferenceEndpoint>) -> MeshInferenceProvider {
    MeshInferenceProvider::with_peer_source(
        weak_local(),
        Arc::new(StubPeerSource { peers }) as Arc<dyn PeerEndpointSource>,
    )
}

// ─── tests ──────────────────────────────────────────────────────

/// RED-FIRST (item 3, the serial half). Four peers, each taking 400 ms
/// to answer its manifest. Serially that is ≥1.6 s of dead time before
/// anything is decided; concurrently it is ~400 ms. On the pre-fix
/// serial loop the elapsed assertion below fails.
#[tokio::test]
async fn four_slow_peers_cost_one_manifest_round_trip_not_four() {
    let mut peers = Vec::new();
    let mut counters = Vec::new();
    for idx in 0..4u128 {
        let hits = Arc::new(AtomicUsize::new(0));
        let addr = spawn_slow_peer(Arc::clone(&hits)).await;
        counters.push(hits);
        peers.push(peer_endpoint(idx, addr));
    }
    let provider = build(peers);

    let started = Instant::now();
    let _ = provider.complete(&mesh_request()).await;
    let elapsed = started.elapsed();

    // Every peer really was asked — otherwise "fast" would just mean
    // "skipped the work", which is the substitution §18.3 warns about.
    for (i, c) in counters.iter().enumerate() {
        assert_eq!(
            c.load(Ordering::SeqCst),
            1,
            "peer-{i} must still be fetched exactly once"
        );
    }

    let serial_floor = MANIFEST_DELAY * 4;
    assert!(
        elapsed < serial_floor,
        "the manifest fan-out is still serial: {elapsed:?} against a {serial_floor:?} \
         serial floor for 4 peers at {MANIFEST_DELAY:?} each. Every 800ms-timeout peer \
         on the mesh adds its full timeout to time-to-first-token."
    );
    // And it is genuinely overlapped, not merely under the floor.
    assert!(
        elapsed < MANIFEST_DELAY * 2,
        "expected ~one round trip of manifest latency, got {elapsed:?}"
    );
}

/// RED-FIRST (item 3, the single-flight half). Two callers arrive at a
/// cold cache at the same instant. `peer_cache` cannot help — neither
/// has an entry to hit — so before the fix each opened its own
/// round-trip and the peer counted 2. It must count 1.
#[tokio::test]
async fn concurrent_callers_share_one_manifest_fetch() {
    let hits = Arc::new(AtomicUsize::new(0));
    let addr = spawn_slow_peer(Arc::clone(&hits)).await;
    let provider = Arc::new(build(vec![peer_endpoint(0, addr)]));

    let a = Arc::clone(&provider);
    let b = Arc::clone(&provider);
    let (_, _) = tokio::join!(
        async move {
            let _ = a.complete(&mesh_request()).await;
        },
        async move {
            let _ = b.complete(&mesh_request()).await;
        }
    );

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "two callers missing the same cold cache entry must share ONE fetch — \
         without in-flight dedup, a slow peer is asked once per concurrent caller, \
         and slowness is exactly what makes callers overlap"
    );
}
