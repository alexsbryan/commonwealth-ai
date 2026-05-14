//! End-to-end test of `ThroughputObservedStream::Drop` →
//! `InferenceReceived` ledger emission on a peer-routed stream.
//!
//! `chat_completion_e2e.rs` covers routing decisions (peer wins
//! OICP scoring, attribution string round-trips, LocalOnly
//! short-circuit). What none of the existing tests pin: when a
//! peer-routed stream actually yields tokens and gets dropped, the
//! contribution ledger sees a matching `InferenceReceived` event
//! for the dimensional balance computation (`commonwealth balance`,
//! `mesh_get_contributions`).
//!
//! Failure mode this guards against: if a future refactor inverts
//! the `count > 0` check in the `Drop` impl, or strips the
//! `ledger_emission` plumbing off the stream wrapper, the ledger
//! goes silent for every peer-routed request and the dimensional
//! balance underreports inference received. That's hard to spot
//! from production logs alone — the request still succeeds, the
//! reply still streams, only the accounting drifts.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::Query;
use axum::response::{sse::Event, IntoResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::NodeId;
use commonwealth_state::{ContributionEmitter, MeshStore};
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
use sovereign_mesh::throughput_tracking::LedgerEmission;

mod common;
use common::TestProvider;

// ── `PeerEndpointSource` stub with a real `ContributionEmitter` ──
//
// Wires a captured ContributionEmitter through `ledger_emission_for`
// so the routing path attaches it to the stream wrapper. After the
// stream drops, the emitter's MeshStore retains the event for the
// assertion to read back.

// ── `PeerEndpointSource` stub with a real `ContributionEmitter` ──
//
// Wires a captured ContributionEmitter through `ledger_emission_for`
// so the routing path attaches it to the stream wrapper. After the
// stream drops, the emitter's MeshStore retains the event for the
// assertion to read back.
struct StubPeerSource {
    peers: Vec<PeerInferenceEndpoint>,
    emitter: ContributionEmitter,
}

#[async_trait]
impl PeerEndpointSource for StubPeerSource {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        self.peers.clone()
    }

    async fn ledger_emission_for(
        &self,
        peer_node_id: &NodeId,
        model_id: &str,
        _peer_name: &str,
    ) -> Option<LedgerEmission> {
        Some(LedgerEmission::new(
            peer_node_id.clone(),
            model_id,
            self.emitter.clone(),
        ))
    }
}

// ── Mock peer HTTP server ──────────────────────────────────────
//
// Same shape as `chat_completion_e2e.rs::spawn_mock_peer` — serves
// a single-slot OICP manifest and a canned SSE stream with three
// chunks. Three chunks lets us also pin that `chunk_count` survives
// past the first arrival into the Drop math.

const PEER_RESPONSE_TEXT: &str = "Peer-served reply.";

#[derive(Deserialize)]
struct StreamQuery {
    #[serde(default)]
    stream: Option<bool>,
}

async fn capabilities_handler() -> impl IntoResponse {
    Json(ProviderManifest {
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
                LatencyClass::Normal,
                32_768,
                4_000,
                0.80,
            )],
        }],
        knowledge: None,
        federation: None,
    })
}

async fn chat_completions_handler(
    Query(q): Query<StreamQuery>,
    Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let _ = q.stream;
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
    // Three chunks so the test can also verify `chunk_count` makes
    // it through to the emission's tokens_generated field.
    let parts = PEER_RESPONSE_TEXT
        .as_bytes()
        .chunks(PEER_RESPONSE_TEXT.len() / 3 + 1)
        .map(|b| std::str::from_utf8(b).unwrap().to_string())
        .collect::<Vec<_>>();
    let events: Vec<std::result::Result<Event, std::convert::Infallible>> = parts
        .iter()
        .map(|p| Ok(Event::default().data(delta(p))))
        .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
        .collect();
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
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

// ── The test ────────────────────────────────────────────────

#[tokio::test]
async fn peer_routed_stream_emits_inference_received_on_drop() {
    // 1. Set up the contribution ledger backed by an in-memory MeshStore.
    let self_node = NodeId::from_u128(0xAAAA_AAAA_AAAA_AAAA);
    let store = MeshStore::in_memory().unwrap();
    let emitter = ContributionEmitter::new(store.clone(), self_node.clone());

    // Sanity: ledger starts empty.
    assert!(
        emitter.events().unwrap().is_empty(),
        "ledger must start empty so we can attribute the new event to the test"
    );

    // 2. Stand up the mock peer (Founder role).
    let peer_addr = spawn_mock_peer().await;
    let base_url = format!("http://{}/v1", peer_addr);

    // 3. Build the stub peer source — one peer, with the emitter
    //    plumbed through `ledger_emission_for` so the routing path
    //    attaches a `LedgerEmission` to the returned stream.
    let peer_node_id = NodeId::from_u128(0xF0F0_F0F0_F0F0_F0F0);
    let peers = vec![PeerInferenceEndpoint {
        node_id: peer_node_id.clone(),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource {
        peers,
        emitter: emitter.clone(),
    });

    // 4. Local stub that loses OICP scoring → request routes to peer.
    let local: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new().with_model_id("qwen2.5-3b-instruct-q4_k_m"),
    );
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    // 5. DeepQuery-shaped request opted into mesh routing.
    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let request = CompletionRequest::new("ping")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);

    // 6. Drive the route → peer → stream path.
    let (mut stream, model_id) = wrapper
        .complete_stream_with_id(&request)
        .await
        .expect("peer route should succeed");

    assert!(
        model_id.contains("@ peer Founder"),
        "expected peer attribution in model_id; got {model_id:?}"
    );

    // 7. Drain the stream fully so chunk_count > 0 inside the wrapper.
    // The legacy `complete_stream_with_id` surface yields
    // `Result<String>` items (not the typed `StreamFrame`); each
    // `Ok(chunk)` increments `chunk_count` in the wrapper's
    // `poll_next` impl.
    let mut collected = String::new();
    while let Some(item) = stream.next().await {
        if let Ok(t) = item {
            collected.push_str(&t);
        }
    }
    assert!(
        collected.contains("Peer-served"),
        "stream must yield the peer's canned text; got: {collected:?}"
    );

    // 8. Drop the stream — Drop spawns the recording task on the
    //    tokio runtime, so we need a tick for it to land.
    drop(stream);
    // The Drop impl `tokio::spawn`s the work; one yield is the
    // bare minimum, but on a loaded box the spawned task may need
    // a moment to acquire the store write lock + serialise the
    // event. 200 ms is comfortable headroom for an in-memory
    // MeshStore without making the test sluggish on CI.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 9. Read the ledger back through the same emitter and assert.
    let events = emitter
        .events()
        .expect("events() must succeed on an in-memory store");
    let received: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            LedgerEventKind::InferenceReceived {
                from_node,
                model_id,
                tokens_generated,
            } => Some((from_node.clone(), model_id.clone(), *tokens_generated)),
            _ => None,
        })
        .collect();

    assert_eq!(
        received.len(),
        1,
        "exactly one InferenceReceived event must land on the ledger; \
         observed events: {events:?}"
    );
    let (from_node, model_id_evt, tokens) = &received[0];
    assert_eq!(
        from_node, &peer_node_id,
        "from_node must be the peer that served the stream"
    );
    assert!(
        !model_id_evt.is_empty(),
        "model_id must be populated; got empty string"
    );
    // chunk_count was 3 deltas + the [DONE] sentinel. The wrapper
    // counts every successful `Ok(chunk)` it forwards through
    // `poll_next`, so the exact count depends on how the SSE bridge
    // de-frames the upstream chunks. We pin `>= 1` (the contract
    // is "at least one chunk → emit") rather than the exact count
    // to avoid coupling the test to bridge internals.
    assert!(
        *tokens >= 1,
        "tokens_generated must reflect ≥1 forwarded chunk; got {tokens}"
    );
}

/// Counterpart invariant: a peer-routed stream that fails to yield
/// even one chunk (degenerate / aborted upstream) must NOT emit an
/// `InferenceReceived` event. The dimensional ledger should report
/// real consumption only.
#[tokio::test]
async fn peer_route_failure_without_chunks_does_not_emit_ledger_event() {
    // Re-use the same scaffolding but point the peer endpoint at
    // a dead address. The wrapper falls through to local, the
    // ThroughputObservedStream never wraps a successful upstream,
    // and the ledger stays empty.
    let self_node = NodeId::from_u128(0xBBBB_BBBB_BBBB_BBBB);
    let store = MeshStore::in_memory().unwrap();
    let emitter = ContributionEmitter::new(store, self_node);

    let dead_peer = vec![PeerInferenceEndpoint {
        node_id: NodeId::from_u128(0xDEAD_BEEF_DEAD_BEEF),
        name: "Ghost".into(),
        // Port 1 is a reserved low port and won't have a listener
        // — connection refuses fast on every OS without dragging
        // out the test.
        base_urls: vec!["http://127.0.0.1:1/v1".into()],
        system_ram_gb: 64,
        benchmark: None,
    }];
    let peer_source: Arc<dyn PeerEndpointSource> = Arc::new(StubPeerSource {
        peers: dead_peer,
        emitter: emitter.clone(),
    });
    let local: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new().with_model_id("qwen2.5-3b-instruct-q4_k_m"),
    );
    let wrapper = MeshInferenceProvider::with_peer_source(local, peer_source);

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(sovereign_core::oicp::ShardingPrivacy::MeshAllowed);
    let request = CompletionRequest::new("ping")
        .with_speed(Speed::Slow)
        .with_oicp(envelope);

    // The wrapper either:
    //   (a) returns a route error (LocalStub::complete_stream
    //       errors after manifest fetch fails), or
    //   (b) yields a stream with zero successful chunks.
    // Either path must not produce a ledger event — `count > 0`
    // is the gate inside Drop.
    let outcome = wrapper.complete_stream_with_id(&request).await;
    if let Ok((mut stream, _model_id)) = outcome {
        while stream.next().await.is_some() {}
        drop(stream);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let events = emitter.events().unwrap();
    let received: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.kind, LedgerEventKind::InferenceReceived { .. }))
        .collect();
    assert!(
        received.is_empty(),
        "zero-chunk peer route must not emit InferenceReceived; \
         observed: {received:?}"
    );
}
