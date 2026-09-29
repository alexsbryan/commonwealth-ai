// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's door to serve's weights: what the serving machine can run,
//! what the catalog offers, what is installed, and the one job that fetches
//! any of it. Every route is serve's (`sovereign_compute::setup_reads`,
//! `sovereign_compute::assets`) and the daemon forwards it
//! (pb-serve-distributes: the process that loads a model is the one that
//! writes it), so clients keep the one address they already dial.
//!
//! # Why these are routes (sv-surface svt-7)
//!
//! The models root belongs to the process serving from it. Until these
//! landed, a client process probed that directory with its own filesystem
//! calls, resolved the catalog with its own copy of `setup_planner`, and wrote
//! into the root with its own downloader — correct only while the two
//! processes shared a host, and a duplicate decider even then (ARCH principle
//! 12: a client asks, it does not own).
//!
//! The four reads are the plan the wizard renders AFTER first run, when a
//! daemon is up to answer them. They deliberately mirror
//! `svrn setup --plan --json`, which is the same four lookups spawned as a
//! process for the case where no daemon exists yet — and both answer the SAME
//! `sovereign_contracts::daemon_wire` types, so the wizard parses one shape
//! either way.
//!
//! The write is one job. Anything that downloads is a job in the IndexBuild
//! pattern (202 + a progress route); a client never awaits a 600 MB fetch on
//! a request thread.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, Path, RawQuery};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::json_error;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// Build the assets router. Merged into the daemon's client router beside
/// `admin_router`, and loopback-guarded for the same reason: these routes
/// write into the serving root and name paths on its disk.
pub fn assets_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/admin/hardware", get(forward_read))
        .route("/v1/admin/setup/catalog", get(forward_read))
        .route("/v1/admin/setup/slot", get(forward_read))
        .route("/internal/ner/model", get(forward_read))
        .route("/v1/admin/assets/download", post(asset_download))
        .route(
            "/v1/admin/assets/download/{job}",
            get(asset_download_progress),
        )
        .localhost_only_with(daemon)
}

/// The reads, forwarded with their query: `GET /v1/admin/hardware`,
/// `/v1/admin/setup/catalog?profile=`, `/v1/admin/setup/slot?kind=&profile=`
/// and `/internal/ner/model`.
async fn forward_read(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    uri: axum::http::Uri,
    RawQuery(raw): RawQuery,
) -> Response {
    let path = with_query(uri.path(), raw);
    forward(&daemon, Method::GET, &path, None).await
}

/// `POST /v1/admin/assets/download` — fetch a model into serve's models
/// root, as a job; the body is relayed as sent.
async fn asset_download(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: Bytes,
) -> Response {
    forward(
        &daemon,
        Method::POST,
        "/v1/admin/assets/download",
        Some(body.to_vec()),
    )
    .await
}

/// `GET /v1/admin/assets/download/{job}` — where that download has got to.
async fn asset_download_progress(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(job_id): Path<String>,
) -> Response {
    let path = format!("/v1/admin/assets/download/{job_id}");
    forward(&daemon, Method::GET, &path, None).await
}

fn with_query(path: &str, raw: Option<String>) -> String {
    match raw {
        Some(q) if !q.is_empty() => format!("{path}?{q}"),
        _ => path.to_string(),
    }
}

/// serve answers on every path (pb-serve-distributes): its status and body
/// are relayed, and an unreachable serve is a named 503, never an answer
/// this process makes up.
async fn forward(
    daemon: &EmbeddedDaemon,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Response {
    let base = daemon.configured_serve_base().await.base;
    match crate::serve_client::forward(&base, method, path, body).await {
        Ok((status, body)) => (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(why) => json_error(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
}

#[cfg(test)]
mod route_tests {
    //! The routes, driven over a real listener in the shape
    //! `daemon::start_daemon` uses, against a stub serve: the half a unit
    //! test cannot see is that every route reaches serve with its method,
    //! path, query and body, and relays what serve said.
    use super::*;
    use sovereign_contracts::setup_config::{DataSection, NodeSection, SetupConfig};
    use std::net::SocketAddr;

    /// The daemon's assets router, dialing `serve` as its `[node] entry`.
    async fn spawn(serve: &str) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = SetupConfig::unconfigured();
        cfg.data = DataSection {
            dir: tmp.path().to_path_buf(),
        };
        cfg.node = NodeSection {
            entry: Some(format!("{serve}/v1")),
            ..NodeSection::default()
        };
        let daemon = crate::daemon::EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            cfg,
            crate::daemon_services::fixtures::headless(),
        );
        std::mem::forget(tmp);
        serve_on_loopback(assets_router(Arc::clone(&daemon))).await
    }

    async fn serve_on_loopback(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let service = app.into_make_service_with_connect_info::<SocketAddr>();
            axum::serve(listener, service).await.ok();
        });
        format!("http://{addr}")
    }

    /// A stub serve that echoes what reached it: method, path, query and
    /// body, with a status the test can tell from a made-up answer.
    async fn stub_serve() -> String {
        let echo = |method: Method, uri: axum::http::Uri, body: Bytes| async move {
            (
                StatusCode::IM_A_TEAPOT,
                serde_json::json!({
                    "method": method.as_str(),
                    "path": uri.path(),
                    "query": uri.query(),
                    "body": String::from_utf8_lossy(&body),
                })
                .to_string(),
            )
        };
        serve_on_loopback(Router::new().fallback(echo)).await
    }

    /// Every route reaches serve and relays its status and body. Failing
    /// input: answer one route in process again, and its row reads that
    /// process's answer instead of the teapot.
    #[tokio::test]
    async fn every_weight_route_reaches_serve_and_relays_its_answer() {
        let base = spawn(&stub_serve().await).await;
        let c = reqwest::Client::new();
        let cases = [
            ("GET", "/v1/admin/hardware", None, ""),
            (
                "GET",
                "/v1/admin/setup/catalog",
                Some("profile=cpu_only"),
                "",
            ),
            ("GET", "/v1/admin/setup/slot", Some("kind=embed"), ""),
            ("GET", "/internal/ner/model", None, ""),
            (
                "POST",
                "/v1/admin/assets/download",
                None,
                r#"{"kind":"gliner"}"#,
            ),
            ("GET", "/v1/admin/assets/download/asset-gguf-x", None, ""),
        ];
        for (method, path, query, body) in cases {
            let url = match query {
                Some(q) => format!("{base}{path}?{q}"),
                None => format!("{base}{path}"),
            };
            let req = match method {
                "POST" => c.post(&url).body(body.to_string()),
                _ => c.get(&url),
            };
            let resp = req.send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 418, "{method} {path} reached serve");
            let seen: serde_json::Value = resp.json().await.unwrap();
            assert_eq!(seen["method"], method, "{path}");
            assert_eq!(seen["path"], path);
            assert_eq!(seen["query"].as_str(), query, "{path}");
            assert_eq!(seen["body"], body, "{path}");
        }
    }

    /// No serve is a named 503, never an answer made up here.
    #[tokio::test]
    async fn a_weight_route_with_no_serve_is_a_named_503() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let gone = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let base = spawn(&gone).await;
        let resp = reqwest::get(format!("{base}/v1/admin/hardware"))
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            resp.text().await.unwrap().contains("not reachable"),
            "the refusal names the unreachable serve"
        );
    }
}
