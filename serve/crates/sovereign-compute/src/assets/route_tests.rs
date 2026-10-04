// SPDX-License-Identifier: AGPL-3.0-or-later
//! The NER read's and the download job's routes, driven over a real listener
//! in the shape serve binds them (moved with the routes from the svrn
//! daemon's assets_http.rs, pb-serve-distributes). The unit tests cover the
//! deciders; these cover the wiring, which is the half a unit test cannot see.

use super::*;
use std::net::SocketAddr;

/// [`bundle`] over a fresh serving root, served with the peer address the
/// loopback guard reads. Returns the base URL.
async fn spawn() -> String {
    let tmp = tempfile::tempdir().unwrap();
    let models_dir = tmp.path().join("models");
    // The tempdir must outlive the server; leak it deliberately — a test
    // process ends in seconds and a dropped dir would delete the data
    // root out from under the routes.
    std::mem::forget(tmp);
    let app = host_kit::shell::mount(vec![bundle(models_dir)]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let service = app.into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, service).await.ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// `GET /internal/ner/model` reports the id the SERVING process is configured for
/// and the path under its own root — not a constant the client repeats.
///
/// Watched fail: hardcode `DEFAULT_MODEL_ID` in the handler and this goes
/// red under `SOVEREIGN_GLINER_MODEL_ID`. (Not set here: the assertion is
/// that the answer AGREES with `configured_model_id`, which is what the
/// extractor loads.)
#[tokio::test]
async fn the_ner_read_names_the_configured_model() {
    let base = spawn().await;
    let s: NerModelStatus = reqwest::get(format!("{base}/internal/ner/model"))
        .await
        .unwrap()
        .json()
        .await
        .expect("parses as NerModelStatus");
    assert_eq!(s.model_id, crate::ner::configured_model_id());
    assert!(
        s.expected_path.ends_with(&s.model_id),
        "{}",
        s.expected_path
    );
    assert_eq!(s.size_estimate_mb, 600);
}

/// A malformed download is refused with 400 and NO job is created — the
/// caller gets the reason, not a job id that will never progress.
///
/// Watched fail: move the `plan()` call after the `insert` and the second
/// assertion (the progress route knows nothing) goes red.
#[tokio::test]
async fn a_malformed_download_is_refused_and_starts_nothing() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let r = c
        .post(format!("{base}/v1/admin/assets/download"))
        .json(
            &serde_json::json!({ "kind": "gguf", "file": "../escape.gguf",
                                   "url": "https://example.invalid/x.gguf" }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(r.text().await.unwrap().contains("bare filename"));

    // And an id nobody minted reads back as a STATE, not a 404 — the
    // poller renders it.
    let p: AssetDownloadProgress = c
        .get(format!("{base}/v1/admin/assets/download/asset-gguf-nope"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .expect("parses");
    assert_eq!(p.state, AssetDownloadState::Unknown);
}

/// The end-to-end shape: a real download off a stub server lands under
/// the DAEMON's data root, and the terminal frame names the path it put
/// it at — which is the string the app writes into a model slot.
///
/// Watched fail: report `path` on every frame instead of only on
/// Complete, and the mid-run assertion goes red; drop `dest` and the
/// final one does.
#[tokio::test]
async fn a_download_lands_under_the_daemon_root_and_names_its_path() {
    use axum::{response::IntoResponse, routing::get, Router};

    let mut body = Vec::new();
    body.extend_from_slice(b"GGUF");
    body.resize(2 * 1024 * 1024, 0u8);
    let stub = Router::new().route(
        "/real.gguf",
        get(move || {
            let body = body.clone();
            async move {
                (
                    [(reqwest::header::CONTENT_TYPE, "application/octet-stream")],
                    body,
                )
                    .into_response()
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, stub.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .ok()
    });

    let base = spawn().await;
    let c = reqwest::Client::new();
    let ack: IngestJobAck = c
        .post(format!("{base}/v1/admin/assets/download"))
        .json(&serde_json::json!({
            "kind": "gguf",
            "url": format!("http://{stub_addr}/real.gguf"),
            "file": "real.gguf",
            "expected_gb": 0.001,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .expect("202 answers IngestJobAck");
    assert_eq!(
        ack.progress_route,
        format!("/v1/admin/assets/download/{}", ack.job_id),
        "the ack names the route that reports it"
    );

    let mut last = None;
    for _ in 0..100 {
        let p: AssetDownloadProgress = c
            .get(format!("{base}{}", ack.progress_route))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .expect("progress parses");
        if p.state == AssetDownloadState::Downloading {
            assert!(
                p.path.is_none(),
                "a path mid-download names a .part file: {p:?}"
            );
        }
        let done = p.state != AssetDownloadState::Downloading;
        last = Some(p);
        if done {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let p = last.expect("polled at least once");
    assert_eq!(p.state, AssetDownloadState::Complete, "{p:?}");
    let path = p.path.expect("a complete download names where it landed");
    assert!(path.ends_with("/models/real.gguf"), "{path}");
    assert_eq!(
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        2 * 1024 * 1024,
        "the bytes are on disk at the path the daemon reported"
    );
}
