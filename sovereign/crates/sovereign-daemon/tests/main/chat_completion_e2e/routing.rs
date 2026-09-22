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
use futures::StreamExt;
use oicp_types::{
    CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass, ModelStatus,
    ProviderManifest, ProviderModel, OICP_VERSION,
};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, Speed};
use sovereign_daemon::daemon::InferenceVenue;
use sovereign_mesh::peer_inference::InferenceRouter;

use super::{
    capabilities_handler, capabilities_handler_fc, local_byom, mip_with_peers, StreamQuery,
    StubVenueSource, PEER_RESPONSE_TEXT,
};

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
    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });

    // 3. Build the local-side provider: a BYOM-class 3B that
    //    cannot satisfy DeepQuery's preferred profile at score
    //    1.0. The `Qwen2.5-3B` base_name is annotated in
    //    `models.toml` as `byom_qwen25.thoughtful` so the OICP
    //    scorer sees real (weak) caps.
    let local: Arc<dyn InferenceProvider> = local_byom();

    // 4. The wrapper under test.
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

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
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed);
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
        InferenceVenue {
            node_id: NodeId::from_u128(1),
            name: "Busy".into(),
            base_urls: vec![format!("http://{busy_addr}/v1")],
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
            inference_availability: None,
            gossip_last_seen_unix: 0,
            pinned_transport: false,
        },
        InferenceVenue {
            node_id: NodeId::from_u128(2),
            name: "Good".into(),
            base_urls: vec![format!("http://{good_addr}/v1")],
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
            inference_availability: None,
            gossip_last_seen_unix: 0,
            pinned_transport: false,
        },
    ];
    let wrapper = mip_with_peers(local_byom(), peers);
    let request = CompletionRequest::new("Is free will compatible with determinism?")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed),
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
    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(7),
        name: "Truncator".into(),
        base_urls: vec![format!("http://{peer_addr}/v1")],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let wrapper = mip_with_peers(local_byom(), peers);
    let request = CompletionRequest::new("Q")
        .with_speed(Speed::Slow)
        .with_oicp(
            InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed),
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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(oicp_types::ShardingPrivacy::LocalOnly);

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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

    // Normal latency is an EXACT class match for the mock peer's
    // Normal-latency claims, so the peer scores at least as well as
    // in `joiner_streams_through_mesh_and_attributes_peer` (which
    // uses adjacent-class Extended and still routes).
    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed);
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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(oicp_types::ShardingPrivacy::LocalOnly);
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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Fast)
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed);
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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed);
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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed);
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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

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

    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![base_url],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let peer_source = Arc::new(StubVenueSource { peers });
    let local: Arc<dyn InferenceProvider> = local_byom();
    let wrapper = InferenceRouter::with_peer_source(
        local,
        peer_source.clone(),
        peer_source,
        Arc::new(sovereign_daemon::slot_manifest::CoreSlotManifest),
    );

    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Extended)
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed);
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
    let peers = vec![InferenceVenue {
        node_id: NodeId::from_u128(42),
        name: "Founder".into(),
        base_urls: vec![format!("http://{}/v1", peer_addr)],
        system_ram_gb: 64,
        benchmark: None,
        current_in_flight: None,
        inference_availability: None,
        gossip_last_seen_unix: 0,
        pinned_transport: false,
    }];
    let wrapper = mip_with_peers(local_byom(), peers);

    // Normal latency + MeshAllowed, model_id LEFT UNSET — the exact
    // shape `build_completion_request` produces for an inbound chat
    // that pins no model.
    let envelope = InferenceRequirements::new()
        .with_hint(CapabilityHint::general())
        .with_latency_class(LatencyClass::Normal)
        .with_sharding(oicp_types::ShardingPrivacy::MeshAllowed);
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
