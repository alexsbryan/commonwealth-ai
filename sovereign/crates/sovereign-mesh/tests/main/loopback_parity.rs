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
//! per-router tests pin the helper.
//!
//! # What this file proves, corrected (2026-09-10)
//!
//! Until now the paragraph above ended "a route added without the
//! middleware (or with a misordered layer stack) would slip past the
//! per-router tests but fail here." **That claim was false and a run
//! falsified it** (`scripts/twin-census.py`, families
//! `mesh-loopback-parity` and `mesh-loopback-spoof`, verdict
//! NOT-A-GATE, commit f6a633519). Re-watched on this tree with
//! `mesh_router`'s `.layer(from_fn(loopback_only))` deleted:
//!
//! ```text
//! mesh_http_rejects_non_loopback_via_mesh_status      ok
//! every_router_fails_closed_when_connect_info_absent  ok
//! loopback_caller_reaches_mesh_status                 ok
//! every_router_refuses_a_request_no_handler_…    FAILED  left: 405  right: 403
//! ```
//!
//! Defence in depth is what blinds the first three: `mesh_status`
//! extracts `ConnectInfo` and calls `enforce_localhost` itself, so the
//! spoofed LAN caller is refused by the HANDLER and the
//! ConnectInfo-less caller 500s in the extractor. Both assertions are
//! over-determined, and the two guards deliberately answer with the
//! same status AND the same body (`{"error":"local-only"}`, one
//! decider), so no body assertion separates them either.
//!
//! `every_router_refuses_a_request_no_handler_of_ours_can_refuse` is
//! the gate — it is the only test here whose red is evidence about the
//! LAYER. Read it before adding a router to this file; the spoof and
//! fail-closed families remain useful as the per-router and
//! fail-closed contracts, but neither is evidence the middleware is
//! mounted.
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
use reqwest::Method;

use sovereign_contracts::types::projection::{project_epistemic_state, project_message_metadata};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::StateStore;
use sovereign_core::types::{Message, Role};
use sovereign_mesh::admin_http::admin_router;
use sovereign_mesh::atlas_http::atlas_router;
use sovereign_mesh::corpus_watch_http::corpus_watch_router;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::features_http::features_router;
use sovereign_mesh::governance_http::governance_router;
use sovereign_mesh::insight_http::insight_router;
use sovereign_mesh::lc_http::lc_router;
use sovereign_mesh::mcp_config_http::mcp_config_router;
use sovereign_mesh::mesh_http::mesh_router;
use sovereign_mesh::meshapp_http::meshapp_router;
use sovereign_mesh::notes_http::notes_router;
use sovereign_mesh::project_http::project_router;
use sovereign_mesh::reading_http::reading_router;
use sovereign_mesh::recipe_project_http::recipe_project_router;
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

/// One router's rejection contract: a non-loopback caller knocking on
/// `path` is refused with 403, whatever the router.
///
/// `exposes` is the half of the message that is NOT mechanical — what a
/// leak on THIS router would hand a LAN caller — and the reason the
/// fifteen callers below stay fifteen NAMED tests rather than one loop
/// over a table: a red still names the router that broke. The fifteen
/// used to spell the spawn, the knock and the assert themselves, 269
/// lines of it, which is fifteen chances for one to drift (ARCH §10.6).
///
/// A `POST` carries an empty JSON body because that is what the handler
/// behind it expects — though on the path this asserts, the layer
/// refuses before any handler reads it.
async fn refused(router: Router, method: Method, path: &str, exposes: &str) {
    let base = spawn_with_spoof(router).await;
    let mut req = reqwest::Client::new().request(method.clone(), format!("{base}{path}"));
    if method == Method::POST {
        req = req.json(&serde_json::json!({}));
    }
    let resp = req.send().await.expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "loopback guard slipped on {path} — {exposes}, and a non-loopback caller got {}",
        resp.status()
    );
}

#[tokio::test]
async fn mesh_http_rejects_non_loopback_via_mesh_status() {
    let (_tmp, d) = fresh_daemon();
    refused(
        mesh_router(d),
        Method::GET,
        "/v1/mesh/status",
        "it names this host's peers",
    )
    .await;
}

#[tokio::test]
async fn admin_http_rejects_non_loopback_via_admin_reload() {
    let (_tmp, d) = fresh_daemon();
    refused(
        admin_router(d),
        Method::POST,
        "/v1/admin/reload",
        "these routes reload the owner's config",
    )
    .await;
}

#[tokio::test]
async fn project_http_rejects_non_loopback_via_list_projects() {
    let (_tmp, rex) = fresh_reindexer();
    refused(
        project_router(rex),
        Method::GET,
        "/v1/projects",
        "the SCIP graph names this host's source trees",
    )
    .await;
}

#[tokio::test]
async fn turn_http_rejects_non_loopback_via_conversation_create() {
    let (_tmp, d) = fresh_daemon();
    refused(
        turn_router(d),
        Method::POST,
        "/v1/conversations",
        "a turn spends THIS host's inference",
    )
    .await;
}

#[tokio::test]
async fn insight_http_rejects_non_loopback_via_insights_list() {
    let (_tmp, d) = fresh_daemon();
    refused(
        insight_router(d),
        Method::GET,
        "/v1/insights",
        "a clip is what the operator kept",
    )
    .await;
}

#[tokio::test]
async fn meshapp_http_rejects_non_loopback_via_graph_read() {
    let (_tmp, d) = fresh_daemon();
    refused(
        meshapp_router(d),
        Method::GET,
        "/internal/meshapp/anything/graph",
        "the explorer reads THIS host's corpus index",
    )
    .await;
}

#[tokio::test]
async fn notes_http_rejects_non_loopback_via_note_query() {
    let (_tmp, d) = fresh_daemon();
    refused(
        notes_router(d),
        Method::POST,
        "/v1/notes/query",
        "notes.db is this operator's working memory",
    )
    .await;
}

#[tokio::test]
async fn features_http_rejects_non_loopback_via_project_list() {
    let (_tmp, d) = fresh_daemon();
    refused(
        features_router(d),
        Method::GET,
        "/v1/features/projects",
        "a charter is the operator's private brief",
    )
    .await;
}

#[tokio::test]
async fn atlas_http_rejects_non_loopback_via_corpora_list() {
    let (_tmp, d) = fresh_daemon();
    refused(
        atlas_router(d),
        Method::GET,
        "/internal/atlas/corpora",
        "the atlas names THIS host's corpora",
    )
    .await;
}

#[tokio::test]
async fn reading_http_rejects_non_loopback_via_chunk_fetch() {
    let (_tmp, d) = fresh_daemon();
    refused(
        reading_router(d),
        Method::GET,
        "/internal/corpus/wikipedia/chunks/0",
        "these routes serve corpus text verbatim",
    )
    .await;
}

#[tokio::test]
async fn corpus_watch_http_rejects_non_loopback_via_list() {
    refused(
        corpus_watch_router(),
        Method::GET,
        "/internal/corpus/watch/list",
        "a watch list names the owner's folders",
    )
    .await;
}

#[tokio::test]
async fn governance_http_rejects_non_loopback_via_view() {
    let (_t, d) = fresh_daemon();
    refused(
        governance_router(d),
        Method::GET,
        "/internal/governance/any/view",
        "these routes APPEND to the owner's oplog",
    )
    .await;
}

#[tokio::test]
async fn mcp_config_http_rejects_non_loopback_via_server_list() {
    let (_t, d) = fresh_daemon();
    refused(
        mcp_config_router(d),
        Method::GET,
        "/v1/mcp/servers",
        "this family WRITES bearer secrets",
    )
    .await;
}

#[tokio::test]
async fn recipe_project_http_rejects_non_loopback_via_project_list() {
    let (_t, d) = fresh_daemon();
    refused(
        recipe_project_router(d),
        Method::GET,
        "/v1/recipe-projects",
        "these routes write the owner's artifact tree",
    )
    .await;
}

#[tokio::test]
async fn lc_http_rejects_non_loopback_via_local_list() {
    refused(
        lc_router(),
        Method::GET,
        "/internal/corpus/local",
        "these routes roll back the OWNER's vault",
    )
    .await;
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

    let (_t6, d6) = fresh_daemon();
    assert_500_on_bare_serve(insight_router(d6), "/v1/insights").await;

    let (_t7, d7) = fresh_daemon();
    assert_500_on_bare_serve(atlas_router(d7), "/internal/atlas/corpora").await;

    let (_t8, d8) = fresh_daemon();
    assert_500_on_bare_serve(meshapp_router(d8), "/internal/meshapp/anything/graph").await;

    let (_t9, d9) = fresh_daemon();
    // The GET on `/v1/notes/{id}` — the router's only GET-shaped read —
    // stands in for the family here: `assert_500_on_bare_serve` drives a
    // GET, and what is being proven is the guard's fail-closed posture,
    // which is per-handler and identical on all six.
    assert_500_on_bare_serve(notes_router(d9), "/v1/notes/any-id").await;

    let (_t10, d10) = fresh_daemon();
    assert_500_on_bare_serve(features_router(d10), "/v1/features/projects").await;

    assert_500_on_bare_serve(lc_router(), "/internal/corpus/local").await;
}

// ── The gate the handler cannot satisfy alone ────────────────────
//
// The two families above are NOT gates, and the twin census proved
// it mechanically (f6a633519). With `mesh_router`'s
// `.layer(from_fn(loopback_only))` deleted the crate still compiles
// and BOTH `mesh_http_rejects_non_loopback_via_mesh_status` and
// `every_router_fails_closed_when_connect_info_absent` still pass:
// `mesh_status` extracts `ConnectInfo` and calls `enforce_localhost`
// itself, so the spoofed LAN caller is 403'd by the HANDLER and the
// ConnectInfo-less caller 500s in the extractor before any handler
// body runs. Both assertions are over-determined — satisfied with
// or without the middleware — and neither can see the layer they
// claim to pin. Defence in depth is exactly what makes them blind:
// the second line of defence answers with the same status, and
// `loopback_only` and `enforce_localhost` deliberately return the
// SAME body (`{"error":"local-only"}`, one decider), so no body
// assertion separates them either.
//
// What no handler can satisfy is a request that never reaches one.
// Drive a method the path does not serve and the request lands on
// axum's `MethodRouter` fallback — a 405 producer that is not any
// of our handlers and never calls `enforce_localhost`. That
// fallback is layered like every other endpoint (axum 0.8.9,
// `routing/method_routing.rs`: `fallback: self.fallback.map(layer_fn)`
// in `MethodRouter::layer`), so `loopback_only` still runs on it.
//
//   with the layer:     spoofed LAN caller -> 403 (the guard)
//   without the layer:  spoofed LAN caller -> 405 (the method fallback)
//
// PUT is the method that reaches the fallback on every path named
// below. It is no longer true that NO route in this crate registers
// PUT — `mcp_config_http`'s `/{name}/token`, `recipe_project_http`'s
// `/{id}/toml` and, since sv-surface D9a, `turn_extras_http`'s
// `/v1/skills/{id}/active` all do — which is why each call site below
// names a path where PUT is NOT a method, and says so where the
// choice is not obvious. The loopback half of each pair asserts the 405 the
// guard is hiding: it proves the 403 came from the middleware and not
// from the route table, so a router that lost its path (rather than
// its layer) cannot pass this test by 404'ing.

/// Serve `router` with real ConnectInfo (no spoof) and return the URL
/// prefix. The sibling of [`spawn_with_spoof`] for assertions that
/// need the unguarded answer.
async fn spawn_plain(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

/// One router's half of the gate. `path` must be a path the router
/// really routes; PUT must not be one of its methods.
async fn assert_the_guard_owns_the_method_fallback(name: &str, router: Router, path: &str) {
    let spoofed = spawn_with_spoof(router.clone()).await;
    let resp = reqwest::Client::new()
        .put(format!("{spoofed}{path}"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "{name}: a non-loopback PUT {path} reaches no handler of ours — \
         only the router's `loopback_only` layer can refuse it. Got {} \
         (405 = the layer is missing or misordered on this router; the \
         per-handler enforce_localhost cannot cover this request)",
        resp.status()
    );

    let plain = spawn_plain(router).await;
    let resp = reqwest::Client::new()
        .put(format!("{plain}{path}"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::METHOD_NOT_ALLOWED,
        "{name}: PUT {path} from loopback must fall through to the method \
         fallback (405) — got {}. A 404 here means the path moved and the \
         403 above proved nothing about THIS router",
        resp.status()
    );
}

#[tokio::test]
async fn every_router_refuses_a_request_no_handler_of_ours_can_refuse() {
    let (_t1, d1) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback("mesh_http", mesh_router(d1), "/v1/mesh/status")
        .await;

    let (_t2, d2) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback("admin_http", admin_router(d2), "/v1/admin/reload")
        .await;

    let (_t3, rex) = fresh_reindexer();
    assert_the_guard_owns_the_method_fallback("project_http", project_router(rex), "/v1/projects")
        .await;

    let (_t4, d4) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "reading_http",
        reading_router(d4),
        "/internal/corpus/status",
    )
    .await;

    assert_the_guard_owns_the_method_fallback(
        "corpus_watch_http",
        corpus_watch_router(),
        "/internal/corpus/watch/list",
    )
    .await;

    let (_t5, d5) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback("turn_http", turn_router(d5), "/v1/conversations")
        .await;

    let (_t5b, d5b) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "turn_extras_http",
        sovereign_mesh::turn_extras_http::turn_extras_router(d5b),
        "/v1/skills",
    )
    .await;

    let (_t5c, d5c) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "documents_http",
        sovereign_mesh::documents_http::documents_router(d5c),
        // NOT `/v1/documents/{id}`: DELETE is a real method there, and
        // `/v1/documents` registers GET only, so PUT reaches the
        // method fallback.
        "/v1/documents",
    )
    .await;

    let (_t5d, d5d) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "corpus_catalog_http",
        sovereign_mesh::corpus_catalog_http::corpus_catalog_router(d5d),
        "/internal/corpus/catalog",
    )
    .await;

    let (_t6, d6) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback("insight_http", insight_router(d6), "/v1/insights")
        .await;

    let (_t7, d7) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "atlas_http",
        atlas_router(d7),
        "/internal/atlas/corpora",
    )
    .await;

    let (_t8, d8) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "meshapp_http",
        meshapp_router(d8),
        "/internal/meshapp/anything/graph",
    )
    .await;

    let (_t9, d9) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback("notes_http", notes_router(d9), "/v1/notes/any-id")
        .await;

    let (_t10, d10) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "features_http",
        features_router(d10),
        "/v1/features/projects",
    )
    .await;

    assert_the_guard_owns_the_method_fallback("lc_http", lc_router(), "/internal/corpus/local")
        .await;

    let (_t11, d11) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "governance_http",
        governance_router(d11),
        "/internal/governance/any/view",
    )
    .await;

    let (_t12, d12) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "mcp_config_http",
        mcp_config_router(d12),
        // NOT `.../{name}/token`: PUT is a real method there, so that path
        // could not distinguish the layer from a handler.
        "/v1/mcp/servers",
    )
    .await;

    let (_t13, d13) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "recipe_project_http",
        recipe_project_router(d13),
        // Same reason: `/{id}/toml` registers PUT.
        "/v1/recipe-projects",
    )
    .await;
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
    // `alpha` is SCOPED and `beta` is not, so the get route's
    // `enabled_corpora` has both a present and an absent case to serve. The
    // pair is the gate: a route hard-coding `None` passes the second alone.
    store
        .set_conversation_enabled_corpora(
            "alpha",
            Some(vec!["sep".to_string(), "wikipedia".to_string()]),
        )
        .await
        .unwrap();
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
                    // The blob rides the route verbatim (svt-3: the desktop
                    // reads its `MessageCompletePayload.metadata` from here
                    // now), so parity means the same blob, not a projection
                    // of it.
                    metadata: m.metadata.clone(),
                }
            })
            .collect(),
        created_at: convo.created_at,
        updated_at: convo.updated_at,
        // Parity means the row's own allow-list, not a projection of it:
        // `enabled_corpora` rides the route verbatim (2026-09-12), because
        // the desktop's `CorpusFilterStrip` renders the chips from it and a
        // route that dropped the field would have rendered every scoped
        // conversation as unscoped.
        enabled_corpora: convo.enabled_corpora.clone(),
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

    // The allow-list rides the row (2026-09-12). Asserted HERE rather than
    // only in the parity equality above because the parity build reads the
    // same field from the same row — so both sides would be `None` together
    // and the equality would pass with the field absent from the envelope.
    // These two assertions are what actually fail when the field is not on
    // the wire (watched: `enabled_corpora` removed from
    // `ConversationResponse` and the handler, `alpha` fails `left: None,
    // right: Some(["sep", "wikipedia"])`; restored after).
    let served_now: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        served_now["enabled_corpora"],
        serde_json::json!(["sep", "wikipedia"]),
        "the scoped conversation's allow-list must ride the get route — \
         the desktop's CorpusFilterStrip renders these chips, and an absent \
         key reads as `all installed corpora`, which is the wrong answer \
         wearing the default's shape"
    );
    let bare: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/beta"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        bare.get("enabled_corpora").is_none(),
        "an unscoped conversation OMITS the key rather than nulling it — \
         same discipline the title carries; got {bare}"
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

/// The two SCOPES a sidebar needs. Until sv-surface this route could only
/// page everything, so the desktop listed from its own `SqliteStateStore` —
/// and in attach that is not the store `create`, `rename` and `delete`
/// write, so a conversation never appeared in the list it was created from.
///
/// The three `skill_id` states are the point: absent pages everything (the
/// server's shape, unchanged), empty is the DEFAULT surface, and a named id
/// is that surface. A route that could not say the middle one would have to
/// serve a scoped sidebar from the unscoped listing, which widens
/// cross-surface visibility silently (§18.3).
#[tokio::test]
async fn conversation_list_route_scopes_by_surface_and_by_corpus() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    // alpha + beta are default-surface. Add one tagged surface row and put
    // an allow-list on alpha so the corpus scope has something to find.
    store
        .insert_empty_conversation("gamma", 300, Some("inner-work"))
        .await
        .unwrap();
    store
        .set_conversation_enabled_corpora("alpha", Some(vec!["sep".to_string()]))
        .await
        .unwrap();
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    };

    // Absent: everything, including the tagged row.
    let all: serde_json::Value = http
        .get(format!("{base}/v1/conversations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut all_ids = ids(&all);
    all_ids.sort();
    assert_eq!(all_ids, vec!["alpha", "beta", "gamma"]);

    // Empty: the DEFAULT surface only — gamma is not a sidebar row.
    let default: serde_json::Value = http
        .get(format!("{base}/v1/conversations?skill_id="))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut default_ids = ids(&default);
    default_ids.sort();
    assert_eq!(
        default_ids,
        vec!["alpha", "beta"],
        "an empty skill_id must mean `skill_id IS NULL`, not 'no scoping'"
    );

    // Named: that surface only.
    let inner: serde_json::Value = http
        .get(format!("{base}/v1/conversations?skill_id=inner-work"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ids(&inner), vec!["gamma"]);

    // Corpus: the default-surface rows whose allow-list names it. beta has
    // a NULL allow-list — everything-scoped, so not one of this notebook's
    // threads — and gamma is the wrong surface.
    let notebook: serde_json::Value = http
        .get(format!("{base}/v1/conversations?corpus_id=sep"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        ids(&notebook),
        vec!["alpha"],
        "an everything-scoped conversation is not a notebook's thread"
    );

    // Two scopes at once is a refusal, not a coin flip.
    let resp = http
        .get(format!("{base}/v1/conversations?skill_id=&corpus_id=sep"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Rename crosses the wire, and the three rules that shape a title travel
/// WITH the write rather than with each surface that offers a rename box
/// (§10.6). The desktop carried its own trim + empty-refusal + 200-char
/// clamp until sv-surface, and in attach mode applied all three to a row
/// the served sidebar never reads — so the old name came back on the next
/// list.
#[tokio::test]
async fn conversation_patch_route_renames_the_row_and_owns_the_title_rules() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    // Padding is trimmed and a 300-character title clamps to 200 — by the
    // HANDLER, so a caller that sends neither rule gets both.
    let padded = format!("  {}  ", "x".repeat(300));
    let resp = http
        .patch(format!("{base}/v1/conversations/alpha"))
        .json(&serde_json::json!({ "title": padded }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(
        resp.text().await.unwrap().is_empty(),
        "204 carries no body — the shape delete already answers"
    );
    let row = store.get_conversation("alpha").await.unwrap();
    assert_eq!(
        row.title.as_deref(),
        Some("x".repeat(200).as_str()),
        "the handler trims and clamps; the row is the proof"
    );

    // And the read route serves what the write landed — one writer, and the
    // list a sidebar renders agrees with it.
    let served: serde_json::Value = http
        .get(format!("{base}/v1/conversations/alpha"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(served["title"], serde_json::json!("x".repeat(200)));

    // An empty title is refused in the host's words rather than written.
    let resp = http
        .patch(format!("{base}/v1/conversations/alpha"))
        .json(&serde_json::json!({ "title": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // A body naming no updatable field is a 400, not a 204 that changed
    // nothing — absence is reported, never defaulted (§18.3).
    let resp = http
        .patch(format!("{base}/v1/conversations/alpha"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // The row survived both refusals intact.
    assert_eq!(
        store
            .get_conversation("alpha")
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("x".repeat(200).as_str()),
    );

    // A conversation this daemon does not hold is the get route's 404.
    let resp = http
        .patch(format!("{base}/v1/conversations/ghost"))
        .json(&serde_json::json!({ "title": "anything" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
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

// ── sv-surface rung 6 commit A: search / memory / insight siblings ─────────
//
// The rung-3 parity discipline extended to the routes the attach boot needs
// beside the CRUD surface: message search (the store's own search decider),
// the memory tombstone/weaken pair (the halving formula moves HERE, off the
// desktop command — one decider), and the insight surface (the same
// `InsightService` the desktop builds, served on loopback). Parity means the
// same thing it meant at rung 3: on the SAME fixture, the route's bytes
// equal the canonical value's bytes — the route adds nothing, drops nothing,
// re-derives nothing.

/// The search route's bytes are the store's `search_messages` rows through
/// the wire envelope — the SAME trait call the desktop's in-process command
/// makes, so wire and local answers cannot drift.
#[tokio::test]
async fn conversation_search_route_serves_the_stores_rows() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/search?q=compatibilism"))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        200,
        "the search route must answer on loopback"
    );
    let served: serde_json::Value = resp.json().await.unwrap();

    // PARITY: expected built in-test from the SAME store call the route
    // makes, capped the way the route caps (50 — the desktop's own cap,
    // moved into the route so it has one home).
    let rows = store.search_messages("compatibilism").await.unwrap();
    let expected = serde_json::json!({
        "results": rows
            .iter()
            .take(50)
            .map(|m| serde_json::json!({
                "content": m.content,
                "conversation_id": m.conversation_id,
            }))
            .collect::<Vec<_>>(),
    });
    assert_eq!(
        served, expected,
        "the search route must serve the store's rows verbatim"
    );
    // And the fixture must have matched at all — §18.4, the instrument
    // validates itself before the equality above can mean anything.
    assert!(
        !served["results"].as_array().unwrap().is_empty(),
        "fixture must match the query or this test proves nothing"
    );

    // A missing q is a 400 naming it, not an empty 200 — an empty result
    // set would be indistinguishable from "nothing matched" (§18.3).
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/conversations/search"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Weaken halves the daemon's own row's confidence (the ONE decider for the
/// formula — it lived in the desktop command until this route), delete
/// tombstones, and a missing id is a 404 that names it.
#[tokio::test]
async fn memory_routes_weaken_tombstone_and_404() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    use sovereign_core::types::Memory;
    let seed = |id: &str, confidence: f64| Memory {
        id: id.to_string(),
        content: format!("memory {id}"),
        source: "conversation_extraction".to_string(),
        confidence,
        created_at: 100,
        last_used: 100,
        ..Memory::default()
    };
    let half = seed("mem-half", 0.8);
    let gone = seed("mem-gone", 0.5);
    futures::future::join_all([store.save_memory(&half), store.save_memory(&gone)]).await;

    // Weaken: the response carries the new confidence AND the daemon's row
    // carries it — the read-modify-write happened server-side, not on a
    // client's snapshot.
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/memories/mem-half/weaken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["confidence"], 0.4, "0.8 halved once");
    let rows = store.get_all_memories().await.unwrap();
    let row = rows.iter().find(|m| m.id == "mem-half").unwrap();
    assert_eq!(row.confidence, 0.4, "the daemon's row was weakened");

    // The floor: a confidence already at 0 stays 0 (max(0.0) is not a
    // negative-zero surprise).
    store.save_memory(&seed("mem-floor", 0.0)).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/memories/mem-floor/weaken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["confidence"], 0.0);

    // Delete: 204 no body, and the row is gone from the daemon's reads.
    let resp = reqwest::Client::new()
        .delete(format!("{base}/v1/memories/mem-gone"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let rows = store.get_all_memories().await.unwrap();
    assert!(
        rows.iter().all(|m| m.id != "mem-gone"),
        "tombstoned memory must leave the recall set"
    );

    // A missing id is the route's own 404 naming the id — not the store's
    // error string and not a silent 204 (§18.3).
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/memories/no-such-memory/weaken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("no-such-memory"),
        "the 404 names the missing id: {body}"
    );
}

/// The insight fixture: a serving daemon whose `ServingCore` carries a REAL
/// `InsightService` — the same construction `daemon_cmd` performs over the
/// daemon's `sovereign.db`, here over a tempdir sqlite the test also holds.
async fn insight_fixture() -> (
    tempfile::TempDir,
    Arc<EmbeddedDaemon>,
    Arc<sovereign_core::insight::InsightService>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let state_store =
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db")).unwrap();
    let insight_store: Arc<dyn sovereign_core::traits::InsightStore> = Arc::new(
        sovereign_store::insight_store::SqliteInsightStore::new(state_store.connection()),
    );
    let service = Arc::new(sovereign_core::insight::InsightService::new(
        insight_store,
        Arc::new(sovereign_core::insight::InsightSinkRegistry::new()),
        // embed is load-bearing here: `clip` embeds the passage before it
        // persists, so the provider must answer embeddings — a bare
        // TestProvider refuses with NotImplemented and the clip 500s.
        Arc::new(TestProvider::new().with_embed_marker(|_| vec![0.5_f32; 4])),
    ));
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            Some(Arc::clone(&service)),
        ),
    );
    (tmp, daemon, service)
}

/// A sink that reports whatever the test told it to. `InsightSink`'s
/// four other methods are unused here — this exists to prove `id`,
/// `display_name` and `connected` CROSS, which is exactly what the
/// desktop's `get_sink_status` could not do: it hard-codes `sinks:
/// vec![]` and answers only the boolean.
struct StubSink {
    id: &'static str,
    display_name: &'static str,
    connected: bool,
}

#[async_trait::async_trait]
impl sovereign_core::traits::InsightSink for StubSink {
    fn id(&self) -> &str {
        self.id
    }
    fn display_name(&self) -> &str {
        self.display_name
    }
    async fn is_connected(&self) -> bool {
        self.connected
    }
    async fn push(
        &self,
        _node: &sovereign_contracts::types::InsightNode,
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn push_batch(
        &self,
        _nodes: &[sovereign_contracts::types::InsightNode],
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
}

/// `GET /v1/insights/sinks` — the route D2 left owed (sv-surface).
///
/// Red-watch 2026-09-10 (run, not asserted): the route line was taken
/// back out of `insight_router` with `sink_status` left in place, and
/// both sink cases failed — this one on a decode EOF (405, empty body,
/// because `/v1/insights/{id}` still matches the path with no GET), the
/// 503 case on `left: 405 right: 503`. `pass: 0 fail: 2`.
///
/// The three SPOOF legs above are deliberately NOT part of that
/// evidence: they exercise the router-level guard, which runs before
/// routing, so an emptied router still answers 403. They guard the
/// posture, not the routes, and each says so by naming the guard.
///
/// Two sinks, one reachable and one not. The assertion that matters is
/// the PER-SINK row: `any_connected` alone is what the desktop command
/// answers today, and it cannot tell a settings pane WHICH vault is
/// down. A route that returned the right boolean with an empty list
/// would pass a boolean-only check and ship the same blind spot.
#[tokio::test]
async fn insight_sink_status_names_each_sink_and_its_reachability() {
    let tmp = tempfile::tempdir().unwrap();
    let state_store =
        sovereign_store::sqlite::SqliteStateStore::open(&tmp.path().join("sovereign.db")).unwrap();
    let insight_store: Arc<dyn sovereign_core::traits::InsightStore> = Arc::new(
        sovereign_store::insight_store::SqliteInsightStore::new(state_store.connection()),
    );
    let mut sinks = sovereign_core::insight::InsightSinkRegistry::new();
    sinks.register(Arc::new(StubSink {
        id: "obsidian",
        display_name: "Obsidian vault",
        connected: true,
    }));
    sinks.register(Arc::new(StubSink {
        id: "logseq",
        display_name: "Logseq graph",
        connected: false,
    }));
    let service = Arc::new(sovereign_core::insight::InsightService::new(
        insight_store,
        Arc::new(sinks),
        Arc::new(TestProvider::new()),
    ));
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            Some(service),
        ),
    );
    let addr = crate::common::spawn_router(insight_router(daemon)).await;

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/v1/insights/sinks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["any_connected"], true,
        "one of the two sinks is reachable: {body}"
    );
    let rows = body["sinks"].as_array().expect("the per-sink list");
    assert_eq!(
        rows.len(),
        2,
        "BOTH registered sinks are named — the desktop command answers an \
         empty list here, which is the blind spot this route closes: {body}"
    );
    let logseq = rows
        .iter()
        .find(|s| s["id"] == "logseq")
        .expect("the unreachable sink is still listed");
    assert_eq!(logseq["display_name"], "Logseq graph");
    assert_eq!(
        logseq["connected"], false,
        "the pane must be able to say WHICH vault is down: {body}"
    );
}

/// A daemon with no insight service answers the named 503 on the sink
/// route too — never `any_connected: false`, which is a CLAIM about
/// sinks and would read as "your vault is disconnected" (ARCH §18.3).
#[tokio::test]
async fn insight_sink_status_without_a_service_is_the_named_503() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            None,
        ),
    );
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/insights/sinks"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("no insight service"),
        "the 503 names the absence: {body}"
    );
}

/// Clip → list → search → delete round-trips through the ONE service, and
/// the wire strips the embedding the way the projection promises.
#[tokio::test]
async fn insight_routes_clip_list_search_delete() {
    let (_tmp, daemon, service) = insight_fixture().await;
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let message_id = uuid::Uuid::new_v4().to_string();
    let resp = client
        .post(format!("{base}/v1/insights/clip"))
        .json(&serde_json::json!({
            "clipped_text": "Compatibilism holds that free will is compatible with determinism.",
            "message_id": message_id,
            "paragraph_index": 3,
            "source": {
                "corpus_id": "sep",
                "article_title": "Free Will",
                "conversation_id": uuid::Uuid::new_v4().to_string(),
            },
            "position": {
                "name": "Compatibilism",
                "style": "Compatibilism",
            },
        }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200, "clip must answer on loopback");
    let raw = resp.text().await.unwrap();
    let clipped: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let id = clipped["insight"]["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());
    assert_eq!(clipped["insight"]["source"]["corpus_id"], "sep");
    // What the clip sent must survive the projection — a field silently
    // dropped here is the wire lying about the row (§18.3).
    assert_eq!(
        clipped["insight"]["position"]["name"], "Compatibilism",
        "the position badge must survive the clip"
    );
    // §18.4 — the projection's own promise: the embedding never crosses.
    assert!(
        !raw.contains("embedding"),
        "the wire projection must strip the embedding: {raw}"
    );

    // A second, non-matching clip — the search assertion below is only
    // discriminating if "everything" and "the query's rows" differ.
    let resp = client
        .post(format!("{base}/v1/insights/clip"))
        .json(&serde_json::json!({
            "clipped_text": "An unrelated note about resawing guitar frets.",
            "message_id": uuid::Uuid::new_v4().to_string(),
            "paragraph_index": 0,
            "source": {
                "corpus_id": null,
                "article_title": null,
                "conversation_id": uuid::Uuid::new_v4().to_string(),
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // LIST parity: the route's rows are the service's rows through the
    // projection — same call, same envelope.
    let resp = client
        .get(format!("{base}/v1/insights"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let served: serde_json::Value = resp.json().await.unwrap();
    let expected = serde_json::to_value(sovereign_mesh::insight_http::InsightListResponse {
        insights: service
            .store
            .list(50)
            .await
            .unwrap()
            .into_iter()
            .map(sovereign_mesh::insight_http::InsightEntry::from)
            .collect(),
    })
    .unwrap();
    assert_eq!(
        served, expected,
        "the list route must serve the service's rows through the projection"
    );

    // SEARCH finds the matching clip by text — and ONLY it: the non-matching
    // second clip stays out, which is what separates "searched" from
    // "listed everything".
    let resp = client
        .get(format!("{base}/v1/insights/search?q=compatibilism"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let served: serde_json::Value = resp.json().await.unwrap();
    let hits = served["insights"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "one of two clips matches the query");
    assert!(
        hits[0]["clipped_text"]
            .as_str()
            .unwrap()
            .contains("Compatibilism"),
        "the hit is the matching clip, not just any row"
    );

    // DELETE is 204 and the row leaves the service's own reads — the second
    // clip stays, which is what makes this a delete and not a wipe.
    let resp = client
        .delete(format!("{base}/v1/insights/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let rows = service.store.list(50).await.unwrap();
    assert_eq!(rows.len(), 1, "one writer, both reads agree");
    assert!(
        rows[0].clipped_text.contains("resawing"),
        "the deleted row is the matching one"
    );

    // A malformed UUID is a 400 naming it, not a 500.
    let resp = client
        .delete(format!("{base}/v1/insights/not-a-uuid"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// A daemon with NO insight service commissioned answers the named 503, not
/// a 404-as-unmounted — "this host built no service" is a fact a client can
/// read (§18.3).
#[tokio::test]
async fn insight_routes_without_a_service_answer_the_named_503() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        crate::common::desktop_services_with_insights(
            engine,
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(TestProvider::new()),
            None,
        ),
    );
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/insights"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("no insight service"),
        "the 503 names the absence: {body}"
    );
}

/// `POST /v1/insights/by-id` — the read `explore_insights` still does
/// in-process (sv-surface D8). Beside its five siblings rather than in
/// `d8_surface_e2e`, because the `InsightService` fixture lives here and a
/// second copy of it would be the twin this campaign deletes.
///
/// The MISSING array is the assertion that earns this route its shape: the
/// store drops ids that name no live row, so a caller comparing lengths
/// learns only that something vanished.
///
/// Watched red 2026-09-10 with ONLY the `/v1/insights/by-id` route removed
/// (the handler and its DTOs left in place, the other five routes still
/// mounted): `loopback_parity.rs:1831` — "the answer decodes", decode EOF
/// at column 0, the empty body of the 404 the method fallback produced.
///
/// The first draft of this watch did not count and is recorded so nobody
/// re-runs it: the clip body omitted `source.conversation_id`, which is a
/// required field, so every clip 422'd and the test could not have passed
/// green either. A red that a correct implementation also produces is not
/// evidence (ARCH §18.1). Fixed, watched green, then re-watched red.
#[tokio::test]
async fn insights_by_id_returns_the_nodes_and_names_the_ones_that_are_gone() {
    let (_tmp, daemon, _service) = insight_fixture().await;
    let addr = crate::common::spawn_router(insight_router(daemon)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Two real clips.
    let mut ids = Vec::new();
    for text in ["Determinism, first passage.", "Compatibilism, second."] {
        let clipped: serde_json::Value = client
            .post(format!("{base}/v1/insights/clip"))
            .json(&serde_json::json!({
                "clipped_text": text,
                "message_id": uuid::Uuid::new_v4().to_string(),
                "paragraph_index": 1,
                "source": {
                    "corpus_id": "sep",
                    "article_title": "Free Will",
                    "conversation_id": uuid::Uuid::new_v4().to_string(),
                },
            }))
            .send()
            .await
            .expect("server reachable")
            .json()
            .await
            .unwrap();
        ids.push(clipped["insight"]["id"].as_str().unwrap().to_string());
    }
    let ghost = uuid::Uuid::new_v4().to_string();

    let body: serde_json::Value = client
        .post(format!("{base}/v1/insights/by-id"))
        .json(&serde_json::json!({ "ids": [ids[0], ghost, ids[1]] }))
        .send()
        .await
        .expect("server reachable")
        .json()
        .await
        .expect("the answer decodes");

    let got = body["insights"].as_array().expect("an insights array");
    assert_eq!(got.len(), 2, "both live rows come back: {body}");
    let returned: std::collections::HashSet<&str> =
        got.iter().filter_map(|n| n["id"].as_str()).collect();
    assert!(
        returned.contains(ids[0].as_str()) && returned.contains(ids[1].as_str()),
        "the two clipped ids are the two returned: {body}"
    );
    assert!(
        got.iter().all(|n| n["embedding"].is_null()),
        "the projection strips the embedding here as it does on list: {body}"
    );
    assert_eq!(
        body["missing"].as_array().map(Vec::len),
        Some(1),
        "the id that named no live row is REPORTED, not silently dropped: {body}"
    );
    assert_eq!(
        body["missing"][0].as_str(),
        Some(ghost.as_str()),
        "and the caller is told WHICH one: {body}"
    );

    // An empty request is a successful empty answer, and both arrays are
    // present — an absent key is indistinguishable from an old host.
    let body: serde_json::Value = client
        .post(format!("{base}/v1/insights/by-id"))
        .json(&serde_json::json!({ "ids": [] }))
        .send()
        .await
        .expect("server reachable")
        .json()
        .await
        .unwrap();
    assert_eq!(body["insights"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["missing"].as_array().map(Vec::len), Some(0));

    // A malformed id is a 400 naming it — one bad id in a batch of thirty
    // is otherwise a mystery.
    let resp = client
        .post(format!("{base}/v1/insights/by-id"))
        .json(&serde_json::json!({ "ids": ["not-a-uuid"] }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not-a-uuid"),
        "the 400 names the id it could not parse: {body}"
    );
}

/// `POST /v1/conversations/{id}/messages/record` appends what the CLIENT
/// authored, verbatim, and drives no turn.
///
/// The route exists for the work the daemon deliberately does not do — a web
/// search the surface ran under its own egress custody, an insight preamble
/// gathered from a local tray. The desktop wrote both through its OWN
/// `SqliteStateStore` until 2026-09-12; on an attached boot that is a
/// different file from the one the sidebar lists, so the exchange landed in a
/// conversation nothing would render it in and reported `Ok`.
///
/// Three things are asserted and each has a distinct way to be wrong: the
/// messages land with the roles and metadata the caller sent (a route that
/// re-derived the role would drop `system`), the HOST minted the ids (a
/// client-chosen id is a second writer of the store's key), and NO extra
/// message appeared — a route that fell through to `collect_turn` would have
/// answered the "query" with the model and written a third row.
#[tokio::test]
async fn record_route_appends_client_authored_messages_without_a_turn() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let before = store
        .get_conversation("alpha")
        .await
        .unwrap()
        .messages
        .len();

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages/record"))
        .json(&serde_json::json!({
            "messages": [
                { "role": "user", "content": "web: compatibilism" },
                {
                    "role": "assistant",
                    "content": "1. Compatibilism\nhttps://example.test\n",
                    "metadata": { "search_backend": "tavily" }
                },
                { "role": "system", "content": "gathered insight preamble" }
            ]
        }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<String> = body["message_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3, "one id per recorded message, in order");

    let after = store.get_conversation("alpha").await.unwrap();
    assert_eq!(
        after.messages.len(),
        before + 3,
        "exactly the three recorded messages — a route that drove a turn \
         would have written a fourth"
    );
    let tail = &after.messages[after.messages.len() - 3..];
    assert_eq!(tail[0].role, Role::User);
    assert_eq!(tail[0].content, "web: compatibilism");
    assert_eq!(tail[1].role, Role::Assistant);
    assert_eq!(
        tail[1]
            .metadata
            .as_ref()
            .and_then(|m| m["search_backend"].as_str()),
        Some("tavily"),
        "the metadata blob is stored verbatim, not projected"
    );
    assert_eq!(
        tail[2].role,
        Role::System,
        "`system` is a role a client may record — the insight preamble is one"
    );
    // The ids the host minted are the ids the store keys on, so a client can
    // name the message it just recorded.
    let stored: Vec<&str> = tail.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(stored, ids.iter().map(|s| s.as_str()).collect::<Vec<_>>());
}

/// An empty `messages` list is a 400, not `{"message_ids": []}`.
///
/// A caller that meant to record an exchange and sent none has a bug, and a
/// success-shaped answer spends the caller's trust instead of their
/// attention (ARCH principle 6). Watched to fail: with the emptiness check
/// removed the route answers 200 and this test reads `left: 200, right:
/// 400`.
#[tokio::test]
async fn record_route_refuses_an_empty_list() {
    let (_tmp, daemon, store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let before = store
        .get_conversation("alpha")
        .await
        .unwrap()
        .messages
        .len();
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages/record"))
        .json(&serde_json::json!({ "messages": [] }))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap_or_default().contains("empty"),
        "the refusal says what was wrong with the request: {body}"
    );
    assert_eq!(
        store
            .get_conversation("alpha")
            .await
            .unwrap()
            .messages
            .len(),
        before,
        "a refused record writes nothing"
    );
}

/// An unknown role is serde's 422, not a string match with a fall-through
/// arm — `role` is the closed set `Role` serialises (ARCH principle 9).
#[tokio::test]
async fn record_route_refuses_an_unknown_role() {
    let (_tmp, daemon, _store) = conversation_fixture(TestProvider::new()).await;
    let addr = crate::common::spawn_router(turn_router(daemon)).await;
    let base = format!("http://{addr}");

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations/alpha/messages/record"))
        .json(&serde_json::json!({
            "messages": [{ "role": "tool", "content": "x" }]
        }))
        .send()
        .await
        .expect("server reachable");
    assert!(
        resp.status().is_client_error(),
        "an unknown role must be refused, not defaulted to `user`; got {}",
        resp.status()
    );
}
