//! `/v1/knowledge/landscape_digest` HTTP-surface test.
//!
//! Pins the daemon-owned endpoint that attached desktops POST to in
//! order to retrieve the prompt-spliced landscape digest blocks the
//! daemon's own `KnowledgeViewManager` would produce.
//!
//! Why this matters: in attach mode, the desktop no longer
//! constructs its own `KnowledgeViewManager` (the daemon owns the
//! enrichment state), so the digest assembly path moved off-process.
//! A regression here breaks every desktop's landscape-grounded
//! conversation — silently, since the splice path is a non-fatal
//! enrichment that just produces less-grounded prompts when missing.
//!
//! Three assertions:
//!
//! 1. **Empty body returns 200 + envelope.** `POST /v1/knowledge/landscape_digest`
//!    with `{}` (the desktop's first warm-fetch shape) returns 200
//!    with the `digests: []` envelope. The body shape is what
//!    `LandscapeDigestResponse` serialises; clients special-case `None`
//!    badly so the field must always be present.
//! 2. **Body fields round-trip.** `active_skill` + `active_is_local_only`
//!    + `conversation_messages` populated → 200, response shape
//!    still well-formed. (The actual digest content depends on
//!    enriched corpora which we don't stand up here; the contract
//!    being pinned is wire-shape, not enrichment quality.)
//! 3. **Loopback enforcement.** Non-127.0.0.1 source → 403. The
//!    `landscape_digest_router` layers `loopback_only` middleware
//!    on top of the per-handler `enforce_localhost` check; this
//!    test pins both via the middleware path.
use std::sync::Arc;

use commonwealth_core::Result as CorpusResult;
use sovereign_mesh::landscape_digest_http::landscape_digest_router;
use sovereign_tools::knowledge_view::KnowledgeViewManager;

mod common;
use common::spawn_router;

/// Build a `KnowledgeViewManager` that compiles + initialises but
/// has no enriched corpora — `compute_digests` will return an empty
/// vector. Sufficient to pin the wire-shape contract; the digest
/// content path is covered by `knowledge_view::manager::tests`.
async fn bare_manager() -> Arc<KnowledgeViewManager> {
    let tmp = tempfile::TempDir::new().unwrap();
    let indexes_dir = tmp.path().join("indexes");
    let recipes_dir = tmp.path().join("recipes");
    let db_path = tmp.path().join("sovereign.db");
    std::fs::create_dir_all(&indexes_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let _ = std::fs::File::create(&db_path).unwrap();
    let embed: corpus_engine::EmbedFn =
        Arc::new(|_| Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 4]) }));
    let infer: corpus_engine::InferenceFn = Arc::new(|_, _: Option<&serde_json::Value>| {
        Box::pin(async { Ok::<String, corpus_engine::Error>("{}".into()) })
    });
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        recipes_dir,
        indexes_dir,
        embed,
    ));
    // Leak the TempDir intentionally — it lives for the test process
    // duration anyway, and Drop ordering across spawn boundaries is
    // fiddly. Per-test temp prefix means no cross-test collision.
    std::mem::forget(tmp);
    Arc::new(KnowledgeViewManager::new(engine, infer, db_path, vec![]).await)
}

/// Sanity that `CorpusResult` is actually re-exported from
/// `commonwealth_core` and the dev-dep chain reaches us.
#[allow(dead_code)]
fn _result_alias_compiles<T>(r: CorpusResult<T>) -> CorpusResult<T> {
    r
}

#[tokio::test]
async fn empty_body_returns_envelope_with_digests_field() {
    // Smallest valid wire request: literally `{}`. Codex-equivalent
    // desktops post this when warming the cache on startup before
    // any active skill is resolved.
    let mgr = bare_manager().await;
    let addr = spawn_router(landscape_digest_router(mgr)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/knowledge/landscape_digest"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("/v1/knowledge/landscape_digest must be reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "empty-body POST should succeed; 4xx here means the request \
         deserialiser regressed past the `default` derive on \
         `LandscapeDigestRequest`"
    );
    let json: serde_json::Value = resp.json().await.unwrap();
    // The shape must include a `digests` array — even when empty —
    // so the desktop doesn't have to special-case `None`. Pinning
    // both the field's presence AND its array-ness so a future
    // serde change can't silently emit `null`.
    assert!(
        json.get("digests").is_some(),
        "response envelope MUST include the `digests` field; got: {json}"
    );
    assert!(
        json["digests"].is_array(),
        "`digests` MUST serialise as an array; got: {json}"
    );
    // With a bare (no-enrichment) manager, the digest list is empty.
    assert_eq!(
        json["digests"].as_array().unwrap().len(),
        0,
        "bare manager (no enriched corpora) should return zero \
         digests, not synthesize placeholder content; got: {json}"
    );
}

#[tokio::test]
async fn full_body_with_active_skill_and_messages_round_trips() {
    // Codex-equivalent: real desktop request with the active skill
    // resolved and a few conversation turns prefilled.
    let mgr = bare_manager().await;
    let addr = spawn_router(landscape_digest_router(mgr)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/knowledge/landscape_digest"))
        .json(&serde_json::json!({
            "active_skill": "deep-research",
            "active_is_local_only": false,
            "conversation_messages": [
                "what's the latest thinking on situated agents?",
                "and how does that intersect with mesh inference routing?",
            ],
        }))
        .send()
        .await
        .expect("/v1/knowledge/landscape_digest must be reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "fully-populated request must succeed; 4xx means the \
         `conversation_messages` array shape or one of the new fields \
         broke the deserialiser"
    );
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(
        json["digests"].is_array(),
        "`digests` MUST serialise as an array even with active_skill \
         set; got: {json}"
    );
}

#[tokio::test]
async fn non_loopback_source_rejected_by_middleware() {
    // The router layers `loopback_only` middleware over the handler.
    // Driving a non-loopback ConnectInfo is hard against a real
    // tokio listener (every connection goes through 127.0.0.1), so
    // this test verifies the easier half: a missing/non-loopback
    // ConnectInfo causes the middleware to fail closed.
    //
    // The full cross-router parity test for the middleware itself
    // lives in `loopback_parity.rs`. Here we just confirm the
    // landscape_digest router participates in that defense — its
    // `landscape_digest_router` constructor layers the middleware
    // by construction.
    //
    // Approach: send a request WITHOUT the `into_make_service_with_connect_info`
    // shape and assert 5xx/4xx. `axum::serve(listener, router)` without
    // `into_make_service_with_connect_info::<SocketAddr>()` causes
    // the `ConnectInfo` extractor inside the handler to fail-closed.
    let mgr = bare_manager().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, landscape_digest_router(mgr)).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/knowledge/landscape_digest"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("/v1/knowledge/landscape_digest must be reachable");
    // Without ConnectInfo wiring, the loopback middleware can't
    // determine source IP and must reject — anything 2xx is a
    // failure of the fail-closed contract.
    assert!(
        !resp.status().is_success(),
        "request to landscape_digest WITHOUT ConnectInfo wiring must \
         not 2xx — the middleware MUST fail closed; got: {}",
        resp.status()
    );
}
