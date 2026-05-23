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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use corpus_engine::ScipGraph;

use sovereign_mesh::admin_http::admin_router;
use sovereign_mesh::corpus_watch_http::corpus_watch_router;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::mesh_http::mesh_router;
use sovereign_mesh::project_http::project_router;
use sovereign_mesh::reading_http::reading_router;
use sovereign_mesh::reindexer::Reindexer;

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
    let daemon = Arc::new(EmbeddedDaemon::new(tmp.path().to_path_buf()));
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
            mesh_router(daemon)
                .into_make_service_with_connect_info::<SocketAddr>(),
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
    assert_500_on_bare_serve(
        reading_router(d4),
        "/internal/corpus/wikipedia/chunks/0",
    )
    .await;

    assert_500_on_bare_serve(
        corpus_watch_router(),
        "/internal/corpus/watch/list",
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
