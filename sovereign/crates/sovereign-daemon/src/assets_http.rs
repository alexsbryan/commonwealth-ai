// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's weights: what this machine can run, what the catalog offers,
//! what is installed, and the one job that fetches any of it.
//!
//! # Why these are routes (sv-surface svt-7)
//!
//! `<data.dir>/models` belongs to the daemon serving from it. Until this
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

use axum::extract::{Extension, Path, Query, RawQuery};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use sovereign_compute::setup_reads;
use sovereign_contracts::daemon_wire::AssetDownloadRequest;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::json_error;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// Build the assets router. Merged into the daemon's client router beside
/// `admin_router`, and loopback-guarded for the same reason: these routes
/// write into the data root and name paths on this disk.
pub fn assets_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/admin/hardware", get(admin_hardware))
        .route("/v1/admin/setup/catalog", get(setup_catalog))
        .route("/v1/admin/setup/slot", get(setup_slot))
        .route("/internal/ner/model", get(ner_model))
        .route("/v1/admin/assets/download", post(asset_download))
        .route(
            "/v1/admin/assets/download/{job}",
            get(asset_download_progress),
        )
        .localhost_only_with(daemon)
}

// ─── The reads ─────────────────────────────────────────────────

pub use sovereign_compute::setup_reads::{HardwareView, ProfileQuery, SlotQuery};

/// `GET /v1/admin/hardware` — what the SERVING machine can run
/// (`sovereign_compute::setup_reads::hardware`). On the dialing path that
/// machine's answer is serve's, forwarded; see [`forward_or`].
async fn admin_hardware(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Response {
    forward_or(&daemon, "/v1/admin/hardware", setup_reads::hardware()).await
}

/// `GET /v1/admin/setup/catalog?profile=` (`setup_reads::catalog`).
async fn setup_catalog(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    RawQuery(raw): RawQuery,
    Query(q): Query<ProfileQuery>,
) -> Response {
    let path = with_query("/v1/admin/setup/catalog", raw);
    forward_or(&daemon, &path, setup_reads::catalog(q)).await
}

/// `GET /v1/admin/setup/slot?kind=fast|embed&profile=` (`setup_reads::slot`).
async fn setup_slot(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    RawQuery(raw): RawQuery,
    Query(q): Query<SlotQuery>,
) -> Response {
    let path = with_query("/v1/admin/setup/slot", raw);
    forward_or(&daemon, &path, setup_reads::slot(q)).await
}

fn with_query(path: &str, raw: Option<String>) -> String {
    match raw {
        Some(q) if !q.is_empty() => format!("{path}?{q}"),
        _ => path.to_string(),
    }
}

/// Where serving lives decides who answers (pb-svrn-dials-serve): on the
/// dialing and hosted paths serve does, and an unreachable serve is a named
/// 503; where no boot decided (tests), this process does.
async fn forward_or(
    daemon: &EmbeddedDaemon,
    path: &str,
    in_process: impl std::future::Future<Output = Response>,
) -> Response {
    if !crate::serve_client::ServingPath::decided().is_some() {
        return in_process.await;
    }
    let base = daemon.configured_serve_base().await.base;
    match crate::serve_client::forward_get(&base, path).await {
        Ok((status, body)) => (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(why) => json_error(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
}


/// `GET /internal/ner/model` — is the NER model installed where the
/// extractor would load it (`sovereign_compute::assets`, serve's weights).
async fn ner_model(_: LocalOnly) -> Response {
    sovereign_compute::assets::ner_model_status()
}

/// `POST /v1/admin/assets/download` — fetch a model into this daemon's
/// models root, as a job (`sovereign_compute::assets::download`).
async fn asset_download(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<AssetDownloadRequest>,
) -> Response {
    sovereign_compute::assets::download(daemon.data_dir().join("models"), req).await
}

/// `GET /v1/admin/assets/download/{job}` — where that download has got to.
async fn asset_download_progress(_: LocalOnly, Path(job_id): Path<String>) -> Response {
    sovereign_compute::assets::download_progress(job_id)
}

#[cfg(test)]
mod route_tests {
    //! The routes, driven over a real listener in the shape
    //! `daemon::start_daemon` uses. The unit tests below cover the deciders;
    //! these cover the wiring, which is the half a unit test cannot see.
    use super::*;
    use sovereign_contracts::daemon_wire::{ProfileName, SlotConfig};
    use sovereign_contracts::setup_config::{DataSection, SetupConfig};
    use std::net::SocketAddr;

    async fn spawn() -> String {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = SetupConfig::unconfigured();
        cfg.data = DataSection {
            dir: tmp.path().to_path_buf(),
        };
        let daemon = crate::daemon::EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            cfg,
            crate::daemon_services::fixtures::headless(),
        );
        // The tempdir must outlive the server; leak it deliberately — a test
        // process ends in seconds and a dropped dir would delete the data
        // root out from under the routes.
        std::mem::forget(tmp);
        let app = assets_router(Arc::clone(&daemon));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let service = app.into_make_service_with_connect_info::<SocketAddr>();
            axum::serve(listener, service).await.ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        format!("http://{addr}")
    }

    /// The three plan reads answer the CONTRACTS types, so a client parses one
    /// vocabulary whether it asked the daemon or spawned `svrn setup --plan`.
    ///
    /// Watched fail: point `setup_catalog` at a different profile than the one
    /// `resolve_profile` returned and the `profile=cpu_only` assertion goes
    /// red (a cpu_only catalog cannot carry a very_high row).
    #[tokio::test]
    async fn the_plan_reads_answer_the_contracts_shapes() {
        let base = spawn().await;
        let c = reqwest::Client::new();

        let hw: HardwareView = c
            .get(format!("{base}/v1/admin/hardware"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .expect("hardware parses as HardwareView");
        assert!(hw.hardware.system_ram_bytes > 0, "the probe ran");
        assert!(ProfileName::ALL.contains(&hw.profile));

        let catalog: Vec<sovereign_contracts::daemon_wire::PrimaryOption> = c
            .get(format!("{base}/v1/admin/setup/catalog?profile=cpu_only"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .expect("catalog parses as Vec<PrimaryOption>");
        assert!(!catalog.is_empty());
        assert!(
            catalog.iter().all(|o| o.profile == "cpu_only"),
            "a cpu_only catalog carries only cpu_only rows: {:?}",
            catalog.iter().map(|o| &o.profile).collect::<Vec<_>>()
        );

        let slot: Option<SlotConfig> = c
            .get(format!(
                "{base}/v1/admin/setup/slot?kind=embed&profile=default"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .expect("slot parses as Option<SlotConfig>");
        assert!(slot
            .expect("default defines an embed slot")
            .file
            .ends_with(".gguf"));
    }

    /// An unrecognised profile or slot kind is a 400 naming what exists — not
    /// a quiet fall back to `default`, which would hand a client a catalog for
    /// a tier it did not ask about.
    ///
    /// Watched fail: replace `ProfileName::from_wire(s).ok_or_else(..)` with
    /// `.unwrap_or(ProfileName::Default)` and both halves go green on 200.
    #[tokio::test]
    async fn an_unknown_profile_or_kind_is_refused_by_name() {
        let base = spawn().await;
        let c = reqwest::Client::new();

        let r = c
            .get(format!("{base}/v1/admin/setup/catalog?profile=gigantic"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::BAD_REQUEST);
        assert!(
            r.text().await.unwrap().contains("very_high"),
            "names the set"
        );

        let r = c
            .get(format!("{base}/v1/admin/setup/slot?kind=thoughtful"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::BAD_REQUEST);
    }

}

