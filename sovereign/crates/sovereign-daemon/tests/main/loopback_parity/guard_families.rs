// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the loopback_parity e2e suite — split from loopback_parity.rs for the §3.2 size ceiling (behaviour-preserving move).

use crate::common::mesh_admin_services;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use reqwest::Method;

use corpus_engine_watchers::reindexer::{Reindexer, ScipGraph};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_daemon::admin_http::admin_router;
use sovereign_daemon::atlas_http::atlas_router;
use sovereign_daemon::corpus_watch_http::corpus_watch_router;
use sovereign_daemon::daemon::EmbeddedDaemon;
use sovereign_daemon::features_http::features_router;
use sovereign_daemon::governance_http::governance_router;
use sovereign_daemon::insight_http::insight_router;
use sovereign_daemon::lc_http::lc_router;
use sovereign_daemon::mcp_config_http::mcp_config_router;
use sovereign_daemon::mesh_http::mesh_router;
use sovereign_daemon::meshapp_http::meshapp_router;
use sovereign_daemon::notes_http::notes_router;
use sovereign_daemon::project_http::project_router;
use sovereign_daemon::reading_http::reading_router;
use sovereign_daemon::recipe_project_http::recipe_project_router;
use sovereign_daemon::turn_http::turn_router;

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
        sovereign_daemon::turn_extras_http::turn_extras_router(d5b),
        "/v1/skills",
    )
    .await;

    let (_t5c, d5c) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "documents_http",
        sovereign_daemon::documents_http::documents_router(d5c),
        // NOT `/v1/documents/{id}`: DELETE is a real method there, and
        // `/v1/documents` registers GET only, so PUT reaches the
        // method fallback.
        "/v1/documents",
    )
    .await;

    let (_t5d, d5d) = fresh_daemon();
    assert_the_guard_owns_the_method_fallback(
        "corpus_catalog_http",
        sovereign_daemon::corpus_catalog_http::corpus_catalog_router(d5d),
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
