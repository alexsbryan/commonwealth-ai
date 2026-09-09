// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! Cross-router loopback parity test.
//!
//! This test exercises both the mesh's loopback-only routers AND the
//! `project_http` / `reindexer` SCIP-graph routers; the latter live
//! behind the `treesitter` feature, so the entire test is gated to
//! match. `cargo test -p sovereign-mesh --features treesitter` runs
//! it; the default `cargo test -p sovereign-mesh` skips it.
//!
//! Every loopback-only router in this crate layers the same
//! `loopback_guard::loopback_only` middleware AND a per-handler
//! `enforce_localhost` call (ARCH §5 defense in depth). The unit
//! tests in `loopback_guard` pin the middleware in isolation; the
//! per-router tests pin the helper. What's missing is a single
//! test that walks **every** router and proves the wiring holds —
//! a route added without the middleware (or with a misordered
//! layer stack) would slip past the per-router tests but fail here.
//!
//! Approach: build each router with minimal deps, wrap it with an
//! `outer` middleware that **spoofs** `ConnectInfo` to a non-loopback
//! socket address before the real `loopback_only` middleware runs.
//! Then hit a representative route and assert 403. This is more
//! reliable than the existing test in `loopback_guard.rs` that
//! depends on a routable interface being present on the host.
//!
//! Layer order: `.layer(outer)` runs *before* the inner router's
//! `.layer(loopback_only)` because axum applies layers in reverse-
//! addition order. So the spoofed `ConnectInfo` is in place by the
//! time `loopback_only` reads it.
use crate::common::mesh_admin_services;
use crate::common::TestProvider;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use corpus_engine_scip::ScipGraph;

use sovereign_contracts::types::projection::{project_epistemic_state, project_message_metadata};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::StateStore;
use sovereign_core::types::{Message, Role};
use sovereign_mesh::admin_http::admin_router;
use sovereign_mesh::corpus_watch_http::corpus_watch_router;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::mesh_http::mesh_router;
use sovereign_mesh::project_http::project_router;
use sovereign_mesh::reading_http::reading_router;
use sovereign_mesh::reindexer::Reindexer;
use sovereign_mesh::turn_http::{
    turn_router, ConversationListEntry, ConversationListResponse, ConversationResponse,
    MessageEntry,
};

/// Outer middleware that overrides `ConnectInfo<SocketAddr>` on the
/// request to a *non-loopback* LAN address. Wraps a real router via
/// `.layer(...)`; the order makes this run before the router's own
/// `loopback_only` middleware, so the guard sees a non-loopback peer
/// even though the test client genuinely connected on 127.0.0.1.
async fn spoof_non_loopback(mut req: Request, next: Next) -> Response {
    let lan: SocketAddr = "192.168.1.42:54321".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(lan));
    next.run(req).await
}

/// Mount `router` with the spoof layer, bind on 127.0.0.1:0, and
/// return the live URL prefix. The listener is wired with
/// `into_make_service_with_connect_info::<SocketAddr>()` so the
/// initial loopback ConnectInfo is present for the spoof middleware
/// to overwrite (the `loopback_only` middleware fails closed without
/// ConnectInfo — that path is covered separately in
/// `loopback_guard::tests::middleware_fails_closed_when_connect_info_missing`).
async fn spawn_with_spoof(router: Router) -> String {
    let outer = router.layer(axum::middleware::from_fn(spoof_non_loopback));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            outer.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

fn fresh_daemon() -> (tempfile::TempDir, Arc<EmbeddedDaemon>) {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    (tmp, daemon)
}

fn fresh_reindexer() -> (tempfile::TempDir, Arc<Reindexer>) {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let merged = Arc::new(ArcSwap::from_pointee(
        ScipGraph::open_in_memory("merged").unwrap(),
    ));
    (tmp, Reindexer::new(indexes, merged))
}

// ── Per-router rejection tests ───────────────────────────────────
//
// One test per loopback-only router. Each picks a route that's
// shaped to short-circuit before the handler body — typically a
// GET with no required body — so we're observing the middleware's
// decision, not the handler's success path.

#[tokio::test]
async fn mesh_http_rejects_non_loopback_via_mesh_status() {
    let (_tmp, daemon) = fresh_daemon();
    let base = spawn_with_spoof(mesh_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "mesh_http loopback guard slipped — non-loopback caller got {}",
        resp.status()
    );
}

#[tokio::test]
async fn admin_http_rejects_non_loopback_via_admin_reload() {
    let (_tmp, daemon) = fresh_daemon();
    let base = spawn_with_spoof(admin_router(daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/admin/reload"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "admin_http loopback guard slipped — non-loopback caller got {}",
        resp.status()
    );
}

#[tokio::test]
async fn project_http_rejects_non_loopback_via_list_projects() {
    let (_tmp, reindexer) = fresh_reindexer();
    let base = spawn_with_spoof(project_router(reindexer)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/projects"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "project_http loopback guard slipped — non-loopback caller got {}",
        resp.status()
    );
}

#[tokio::test]
async fn turn_http_rejects_non_loopback_via_conversation_create() {
    let (_tmp, daemon) = fresh_daemon();
    let base = spawn_with_spoof(turn_router(daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "turn_http loopback guard slipped — a turn runs THIS host's tools \
         against THIS host's corpora, and a non-loopback caller got {}",
        resp.status()
    );
}

#[tokio::test]
async fn reading_http_rejects_non_loopback_via_chunk_fetch() {
    let (_tmp, daemon) = fresh_daemon();
    let base = spawn_with_spoof(reading_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/internal/corpus/wikipedia/chunks/0"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "reading_http loopback guard slipped — non-loopback caller got {}",
        resp.status()
    );
}

#[tokio::test]
async fn corpus_watch_http_rejects_non_loopback_via_list() {
    let base = spawn_with_spoof(corpus_watch_router()).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/internal/corpus/watch/list"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "corpus_watch_http loopback guard slipped — non-loopback caller got {}",
        resp.status()
    );
}

// ── Negative control: loopback callers still reach handlers ──────
//
// Without the spoof middleware, a real loopback caller should NOT
// get a 403 — that would mean the guard is over-rejecting and the
// rejection tests above are firing on noise. We pick one route
// (mesh_status) that returns 200 on a no-mesh daemon, so we can
// assert a clean success.

#[tokio::test]
async fn loopback_caller_reaches_mesh_status() {
    let (_tmp, daemon) = fresh_daemon();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            mesh_router(daemon).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/mesh/status"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "loopback caller must NOT be 403'd on /v1/mesh/status; \
         got {} — the loopback guard is over-rejecting",
        resp.status()
    );
}

// ── Cross-router invariant: middleware fails closed when ConnectInfo missing ─
//
// `loopback_guard::tests::middleware_fails_closed_when_connect_info_missing`
// already pins this for ONE router. Walk every router to prove the
// fail-closed contract holds uniformly — a future router that
// forgets to apply the middleware would either over-permit (200
// without auth) or under-protect (5xx with bad UX). Both fail.

#[tokio::test]
async fn every_router_fails_closed_when_connect_info_absent() {
    // Build each router and serve it WITHOUT
    // `into_make_service_with_connect_info` — that's the production
    // failure mode the middleware's INTERNAL_SERVER_ERROR branch
    // defends against. Each must respond 500, not 200, not 403.
    async fn assert_500_on_bare_serve(router: Router, path: &str) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // NOTE: bare axum::serve — no connect_info.
            let _ = axum::serve(listener, router).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let resp = reqwest::Client::new()
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .expect("server reachable");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "router serving {path} must fail closed (500) when ConnectInfo is absent; \
             got {} instead — middleware is missing or misordered",
            resp.status()
        );
    }

    let (_t1, d1) = fresh_daemon();
    assert_500_on_bare_serve(mesh_router(d1), "/v1/mesh/status").await;

    let (_t2, d2) = fresh_daemon();
    // admin_reload is POST-only, but the loopback middleware runs
    // for every method; a GET is rejected by the route matcher
    // BEFORE the middleware. We use POST with no body for the
    // route to reach the middleware.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, admin_router(d2)).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/admin/reload"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "admin_router must fail closed (500) when ConnectInfo absent; got {}",
        resp.status()
    );

    let (_t3, rex) = fresh_reindexer();
    assert_500_on_bare_serve(project_router(rex), "/v1/projects").await;

    let (_t4, d4) = fresh_daemon();
    assert_500_on_bare_serve(reading_router(d4), "/internal/corpus/wikipedia/chunks/0").await;

    assert_500_on_bare_serve(corpus_watch_router(), "/internal/corpus/watch/list").await;

    let (_t5, d5) = fresh_daemon();
    assert_500_on_bare_serve(turn_router(d5), "/v1/conversations").await;
}

// Silence unused-import lint when the test build slims something
// out — keeps the file robust to future feature flags without
// having to chase one-off `#[allow]` annotations.
#[allow(dead_code)]
fn _silence_unused() -> PathBuf {
    PathBuf::new()
}

// ── sv-surface rung 1: the corpus-status route serves the one decider ────
//
// The parity instrument for the corpus-status family: the route's bytes and
// `scan_corpus_rows`'s bytes on the SAME fixture are equal, so the wire's
// answer and the CLI's printed answer cannot drift apart. The CLI prints
// from the same function (sovereign-cli-llm corpus_cmd/status.rs imports it
// since rung 1); before the rung it walked the indexes dir privately while
// the desktop walked it through `installed_indexes` — the §10.6 twin.

/// One minimal READY corpus + one in-flight PARTITION, on disk, under the
/// engine's index dir — the two states a status surface must never confuse
/// (a partition named as a corpus is the 2026-08-12 regression recorded in
/// the decider's tests).
fn write_fixture_meta(dir: &std::path::Path, corpus_id: &str, ingestion_in_progress: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let meta = serde_json::json!({
        "corpus_id": corpus_id,
        "corpus_name": format!("{corpus_id} (fixture)"),
        "embedding_model": "qwen-embedding-0.6b",
        "embedding_dimensions": 1024,
        "mesh_sharing": false,
        "license": "private",
        "created_at": 1_786_548_248_u64,
        "last_updated": 1_786_548_248_u64,
        "schema_version": 3,
        "is_shard": false,
        "ingestion_in_progress": ingestion_in_progress,
        "indexes_built": !ingestion_in_progress,
    });
    std::fs::write(
        corpus_engine::Corpus::meta_in(dir),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn corpus_status_route_serves_the_one_deciders_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let indexes = tmp.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    write_fixture_meta(&indexes.join("ready-corpus"), "ready-corpus", false);
    write_fixture_meta(
        &indexes.join("building-corpus-partition-node-1"),
        "building-corpus",
        true,
    );

    // A daemon whose ServingCore carries a REAL engine over that fixture —
    // through THE assembler, like every production site.
    let engine = std::sync::Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        indexes.clone(),
        std::sync::Arc::new(|_t: &str| {
            Box::pin(async { Ok(vec![0.0_f32; 8]) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = corpus_engine::Result<Vec<f32>>> + Send>,
                >
        }),
    ));
    let daemon = EmbeddedDaemon::in_memory(
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_engine(engine),
    );
    let addr = crate::common::spawn_router(reading_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/internal/corpus/status"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        200,
        "the status route must answer on loopback"
    );
    let served: serde_json::Value = resp.json().await.expect("rows serialize as JSON");

    // PARITY, pinned: the route's bytes are the decider's bytes on the same
    // fixture — not merely "also correct", but THE SAME value.
    let rows = corpus_engine::engine::status::scan_corpus_rows(&indexes).unwrap();
    let printed: serde_json::Value = serde_json::to_value(&rows).unwrap();
    assert_eq!(
        served, printed,
        "the route and `svrn corpus status` must serve one decider's rows"
    );

    // And the fixture's two states survive the wire with their labels —
    // the spelling the CLI-contract journey greps for. (Rows arrive in
    // `corpus_id` order — the decider's BTreeMap — so look them up rather
    // than trusting fixture-write order.)
    let by_id = |v: &serde_json::Value, id: &str| {
        v.as_array()
            .expect("rows serve as an array")
            .iter()
            .find(|r| r["corpus_id"] == id)
            .unwrap_or_else(|| panic!("no row for {id} in {v}"))
            .clone()
    };
    let ready = by_id(&served, "ready-corpus");
    assert_eq!(ready["state_label"], "ready");
    let building = by_id(&served, "building-corpus");
    assert_eq!(building["state_label"], "building");
}

// ── sv-surface rung 3: the conversation CRUD routes serve the store's ────
// ── rows through the canonical projections ───────────────────────────────
//
// The parity instrument for the conversation family. sovereign-server's
// routes.rs is the CONTRACT (mobile already speaks it); these daemon routes
// mirror its envelopes field-for-field and call the SAME projection deciders
// the server calls (`sovereign_contracts::types::projection`). Parity here
// means: on the SAME fixture rows, the route's bytes equal the wire schema's
// serialization of those rows — the route adds nothing, drops nothing, and
// re-derives nothing. The desktop's in-process reads answer from the same
// `StateStore` rows through the same trait methods, so one-decider-on-the-row
// is what this pins; the desktop's HTTP conversion is rung 6.

/// A seeded conversation row: one user turn, one assistant turn whose metadata
/// projects to provenance + citations + an epistemic ledger. The assistant
/// metadata is the interesting half — it is what separates "serves the row"
/// from "serves the row's projection faithfully".
async fn seed_conversation(
    store: &Arc<dyn StateStore>,
    id: &str,
    title: Option<&str>,
    with_metadata: bool,
) {
    let m1 = Message {
        id: format!("{id}-m1"),
        conversation_id: id.to_string(),
        role: Role::User,
        content: "what is compatibilism?".to_string(),
        created_at: 100,
        metadata: None,
        version: 0,
    };
    let m2 = Message {
        id: format!("{id}-m2"),
        conversation_id: id.to_string(),
        role: Role::Assistant,
        content: "Compatibilism holds that free will is compatible with determinism.".to_string(),
        created_at: 101,
        metadata: with_metadata.then(|| {
            serde_json::json!({
                "provenance": {
                    "inference_backend": "test-provider",
                    "coarse_intent": "DeepQuery",
                    "total_latency_ms": 42,
                    "sources": [{ "origin": "sep", "count": 2 }]
                },
                "retrieved_chunks": [{
                    "corpus_id": "sep",
                    "chunk_id": 7,
                    "snippet": "Compatibilism holds that...",
                    "score": 0.9,
                    "title": "Free Will"
                }],
                "epistemic_state": {
                    "version": 1,
                    "demands": [],
                    "holdings": [],
                    "gaps": [],
                    "verdict": "grounded",
                    "citations": []
                }
            })
        }),
        version: 0,
    };
    store
        .insert_empty_conversation(id, 100, None)
        .await
        .unwrap();
    store.save_message(&m1).await.unwrap();
    store.save_message(&m2).await.unwrap();
    if let Some(t) = title {
        store.update_conversation_title(id, t).await.unwrap();
    }
}

/// The rung-3 fixture: a serving daemon over a store the test also holds —
/// the same shape `turn_surface.rs` uses, so the CRUD routes are exercised
/// against the exact wiring a turn already runs on.
async fn conversation_fixture(
    provider: TestProvider,
) -> (tempfile::TempDir, Arc<EmbeddedDaemon>, Arc<dyn StateStore>) {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let services =
        crate::common::desktop_services_with_store(engine, Arc::clone(&store), Arc::new(provider));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    );
    seed_conversation(&store, "alpha", Some("Free will"), true).await;
    seed_conversation(&store, "beta", None, false).await;
    (tmp, daemon, store)
}

/// The list route's bytes are the wire schema's serialization of
/// `store.list_conversations` — same query, same rows, same envelope.
#[tokio::test]
async fn conversation_list_route_serves_the_stores_rows() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200, "the list route must answer on loopback");
    let raw = resp.text().await.unwrap();

    // PARITY: expected is built in-test from the SAME store call the route
    // makes, through the wire type — anything the route re-derived or dropped
    // shows up as a byte difference.
    let rows = store.list_conversations(20, 0).await.unwrap();
    let expected = serde_json::to_value(ConversationListResponse {
        conversations: rows
            .iter()
            .map(|c| ConversationListEntry {
                id: c.id.clone(),
                title: c.title.clone(),
                created_at: c.created_at,
                updated_at: c.updated_at,
            })
            .collect(),
    })
    .unwrap();
    let served: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        served, expected,
        "the list route must serve the store's rows through the wire envelope"
    );

    // Membership + the two shapes that must never blur: a titled row carries
    // its title, an untitled row OMITS the key (sovereign-server's
    // skip_serializing_if — `"title": null` is the rung-4 drift, caught there
    // only because a test like this one did not exist for reading).
    let ids: Vec<&str> = served["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"alpha") && ids.contains(&"beta"),
        "ids: {ids:?}"
    );
    assert!(
        !raw.contains("\"title\":null") && !raw.contains("\"title\": null"),
        "an untitled conversation must omit the title key, not null it — got {raw}"
    );

    // The server's pagination defaults are the route's: limit/offset reach
    // the store call verbatim.
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations?limit=1&offset=0"))
        .send()
        .await
        .unwrap();
    let served: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(served["conversations"].as_array().unwrap().len(), 1);
}

/// The get route's bytes are the canonical projections of the store's row —
/// the same deciders sovereign-server's `get_conversation` calls, so a
/// resumed conversation renders identically from either host.
#[tokio::test]
async fn conversation_get_route_serves_the_canonical_projection() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/alpha"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200);
    let raw = resp.text().await.unwrap();

    // PARITY: expected is built in-test by running the CONTRACTS-LAYER
    // projections over the store's row — not by parsing what the route
    // served.
    let convo = store.get_conversation("alpha").await.unwrap();
    let expected = serde_json::to_value(ConversationResponse {
        id: "alpha".to_string(),
        title: convo.title.clone(),
        messages: convo
            .messages
            .iter()
            .map(|m| {
                let role = m.role_str().to_string();
                let (provenance, citations) = project_message_metadata(&m.metadata);
                MessageEntry {
                    id: m.id.clone(),
                    role,
                    content: m.content.clone(),
                    created_at: m.created_at,
                    provenance,
                    citations,
                    epistemic_state: project_epistemic_state(&m.metadata),
                }
            })
            .collect(),
        created_at: convo.created_at,
        updated_at: convo.updated_at,
    })
    .unwrap();

    // §18.4 — validate the instrument before trusting the equality: the
    // fixture's metadata MUST project to Some/Some/nonempty, else both sides
    // are None and the test passes vacuously.
    let assistant = &expected["messages"][1];
    assert!(
        assistant.get("provenance").is_some(),
        "fixture must project provenance, else this test proves nothing"
    );
    assert!(
        !assistant["citations"].as_array().unwrap().is_empty(),
        "fixture must project citations"
    );
    assert!(
        assistant.get("epistemic_state").is_some(),
        "fixture must project an epistemic ledger"
    );

    let served: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        served, expected,
        "the get route must serve the contracts-layer projection of the row"
    );

    // A bare row serves bare: beta has no metadata, so its wire form carries
    // neither provenance nor citations keys at all (absent stays absent).
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap();
    let raw = resp.text().await.unwrap();
    assert!(
        !raw.contains("provenance") && !raw.contains("citations") && !raw.contains("title"),
        "a bare row's wire form must omit every optional key — got {raw}"
    );
}

/// A missing row answers the server's exact 404 sentence — not a generic
/// daemon error, which would be a byte a client could tell hosts apart by.
#[tokio::test]
async fn conversation_get_missing_is_the_servers_404() {
    let (_tmp, daemon, _store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/conversations/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "error": "Conversation not found" }),
        "the 404 body is sovereign-server's spelling (routes.rs)"
    );
}

/// Delete is the server's 204-no-body, and the row is really gone — the
/// read routes answer 404 for it afterward, through the shared store.
#[tokio::test]
async fn conversation_delete_route_is_204_and_removes_the_row() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .delete(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(
        resp.text().await.unwrap().is_empty(),
        "204 carries no body (the server's spelling)"
    );

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // And the list still serves parity against the store AFTER the delete —
    // one writer, both reads agree.
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations"))
        .send()
        .await
        .unwrap();
    let served: serde_json::Value = resp.json().await.unwrap();
    let rows = store.list_conversations(20, 0).await.unwrap();
    let expected = serde_json::to_value(ConversationListResponse {
        conversations: rows
            .iter()
            .map(|c| ConversationListEntry {
                id: c.id.clone(),
                title: c.title.clone(),
                created_at: c.created_at,
                updated_at: c.updated_at,
            })
            .collect(),
    })
    .unwrap();
    assert_eq!(served, expected);
}

/// The one-shot messages route drives the SAME driver the WebSocket stream
/// runs (`collect_turn` → `serve_turn`), and the turn it wrote is what the
/// get route then serves — one writer, REST and WS and read all agreeing.
#[tokio::test]
async fn conversation_messages_route_serves_one_collect_turn() {
    let provider =
        TestProvider::new().with_stream_chunks(vec!["one ".to_string(), "two".to_string()]);
    let (_tmp, daemon, store) = conversation_fixture(provider).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages"))
        .json(&serde_json::json!({ "content": "hello" }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200, "the one-shot turn route must answer");
    let reply: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(reply["role"], "assistant", "the server's spelling");
    assert_eq!(reply["content"], "one two");
    let message_id = reply["message_id"].as_str().unwrap().to_string();
    assert!(!message_id.is_empty());

    // The reply IS the persisted row — the route did not fabricate an answer
    // shape the store never saw.
    let convo = store.get_conversation("alpha").await.unwrap();
    let persisted = convo
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .unwrap_or_else(|| panic!("message {message_id} not persisted"));
    assert_eq!(persisted.content, "one two");

    // And the get route serves it: 2 seeded + 1 user + 1 assistant.
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/alpha"))
        .send()
        .await
        .unwrap();
    let served: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(served["messages"].as_array().unwrap().len(), 4);
}
