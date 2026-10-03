// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/v1/projects/*`'s loopback guard, moved with the router from
//! sovereign-daemon's loopback_parity suite (guard_families.rs, the
//! `project_http` family; pb-code-daemon-exit). The same three contracts
//! every loopback-only router is held to there:
//!
//! - a non-loopback caller is refused with 403;
//! - served without `ConnectInfo`, the router fails closed with 500;
//! - a non-loopback `PUT`, which no handler routes, is refused by the layer
//!   (403), while the same `PUT` from loopback reaches the method fallback
//!   (405), so the 403 came from the guard and not from a missing path.
#![cfg(feature = "treesitter")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use corpus_engine_watchers::reindexer::{Reindexer, ScipGraph};
use sovereign_code::project_http::project_router;

/// Overrides `ConnectInfo` with a LAN address before the router's own
/// `loopback_only` layer runs, so the guard sees a non-loopback peer.
async fn spoof_non_loopback(mut req: Request, next: Next) -> Response {
    let lan: SocketAddr = "192.168.1.42:54321".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(lan));
    next.run(req).await
}

/// Serve `router` on 127.0.0.1:0 and return its URL prefix: with
/// `ConnectInfo` (and the spoof layer when `spoof`), or bare.
async fn spawn(router: Router, connect_info: bool, spoof: bool) -> String {
    let router = if spoof {
        router.layer(axum::middleware::from_fn(spoof_non_loopback))
    } else {
        router
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = if connect_info {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        } else {
            axum::serve(listener, router).await
        };
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
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

#[tokio::test]
async fn project_http_rejects_non_loopback_via_list_projects() {
    let (_tmp, rex) = fresh_reindexer();
    let base = spawn(project_router(rex), true, true).await;
    let resp = reqwest::get(format!("{base}/v1/projects")).await.unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "loopback guard slipped on /v1/projects — the SCIP graph names this host's \
         source trees, and a non-loopback caller got {}",
        resp.status()
    );
}

#[tokio::test]
async fn project_http_fails_closed_when_connect_info_absent() {
    let (_tmp, rex) = fresh_reindexer();
    let base = spawn(project_router(rex), false, false).await;
    let resp = reqwest::get(format!("{base}/v1/projects")).await.unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "project_router must fail closed (500) when ConnectInfo is absent; got {}",
        resp.status()
    );
}

#[tokio::test]
async fn project_http_guard_owns_the_method_fallback() {
    let (_tmp, rex) = fresh_reindexer();
    let router = project_router(rex);
    let spoofed = spawn(router.clone(), true, true).await;
    let resp = reqwest::Client::new()
        .put(format!("{spoofed}/v1/projects"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a non-loopback PUT /v1/projects reaches no handler — only the router's \
         `loopback_only` layer can refuse it. Got {} (405 = the layer is missing \
         or misordered)",
        resp.status()
    );
    let plain = spawn(router, true, false).await;
    let resp = reqwest::Client::new()
        .put(format!("{plain}/v1/projects"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::METHOD_NOT_ALLOWED,
        "PUT /v1/projects from loopback must fall through to the method fallback \
         (405) — got {}. A 404 means the path moved and the 403 above proved nothing",
        resp.status()
    );
}
