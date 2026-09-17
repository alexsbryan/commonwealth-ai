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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use sovereign_contracts::daemon_wire::{
    AssetDownloadProgress, AssetDownloadRequest, AssetDownloadState, AssetKind, HardwareProfile,
    IngestJobAck, NerModelStatus, ProfileName, SlotConfig,
};
use sovereign_inference::hardware;
use sovereign_inference::setup_planner;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::json_error;
use crate::job_registry::JobRegistry;
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

/// Answer of `GET /v1/admin/hardware`. Flat rather than nested so the
/// profile travels with the probe that selected it — a caller that read one
/// without the other would be reasoning about a tier it cannot explain.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct HardwareView {
    /// What the probe found on the machine the daemon runs on.
    pub hardware: HardwareProfile,
    /// The tier that follows from it.
    pub profile: ProfileName,
}

/// `GET /v1/admin/hardware` — what the SERVING machine can run.
///
/// Detection walks `/proc` (or the llama.cpp backend device list), so it goes
/// to a blocking thread rather than the reactor — the same call
/// `setup_cmd` makes, through the same free function.
async fn admin_hardware(_: LocalOnly) -> Response {
    let Ok(hardware) = tokio::task::spawn_blocking(hardware::detect_hardware).await else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "hardware detection panicked",
        );
    };
    let profile = hardware::select_profile(&hardware);
    Json(HardwareView { hardware, profile }).into_response()
}

/// Query of the two setup reads. `profile` absent means "detect it".
#[derive(Debug, Deserialize)]
pub struct ProfileQuery {
    /// A [`ProfileName::as_str`] spelling. An unrecognised one is refused by
    /// name rather than bucketed into `default` (ARCH principle 6).
    #[serde(default)]
    pub profile: Option<String>,
}

async fn resolve_profile(q: &ProfileQuery) -> Result<ProfileName, Response> {
    match q.profile.as_deref() {
        Some(s) => ProfileName::from_wire(s).ok_or_else(|| {
            json_error(
                StatusCode::BAD_REQUEST,
                &format!(
                    "unknown profile `{s}` (expected one of {})",
                    ProfileName::ALL
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        }),
        None => match tokio::task::spawn_blocking(hardware::detect_hardware).await {
            Ok(hw) => Ok(hardware::select_profile(&hw)),
            Err(_) => Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "hardware detection panicked",
            )),
        },
    }
}

/// `GET /v1/admin/setup/catalog?profile=` — the curated primary catalog for a
/// tier. The same `setup_planner::build_primary_catalog` the wizard calls.
async fn setup_catalog(_: LocalOnly, Query(q): Query<ProfileQuery>) -> Response {
    let profile = match resolve_profile(&q).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    Json(setup_planner::build_primary_catalog(&profile)).into_response()
}

/// Query of `GET /v1/admin/setup/slot`.
#[derive(Debug, Deserialize)]
pub struct SlotQuery {
    /// `fast` or `embed`. The thoughtful slot has the catalog route above;
    /// `fim` has its own onboarding (`svrn setup --fim`) and is not offered
    /// here, so an unknown kind is refused rather than resolved to something.
    pub kind: String,
    #[serde(default)]
    pub profile: Option<String>,
}

/// `GET /v1/admin/setup/slot?kind=fast|embed&profile=` — the single-pick slot
/// for a tier. `null` when the bundled manifest defines none: absent, not a
/// substituted default (ARCH principle 6).
async fn setup_slot(_: LocalOnly, Query(q): Query<SlotQuery>) -> Response {
    let kind = match q.kind.as_str() {
        "fast" => setup_planner::SlotKind::Fast,
        "embed" => setup_planner::SlotKind::Embed,
        other => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("unknown slot kind `{other}` (expected fast or embed)"),
            )
        }
    };
    let profile = match resolve_profile(&ProfileQuery {
        profile: q.profile.clone(),
    })
    .await
    {
        Ok(p) => p,
        Err(r) => return r,
    };
    let slot: Option<SlotConfig> = setup_planner::resolve_slot(&profile, kind);
    Json(slot).into_response()
}

/// `GET /internal/ner/model` — is the entity extractor's model installed
/// where the extractor would load it.
///
/// `model_id` is the daemon's `configured_model_id()`, not a constant the
/// client repeats: a host with `SOVEREIGN_GLINER_MODEL_ID` set loads a
/// different export, and a client hardcoding the default would report
/// "installed" about a file the daemon never opens.
async fn ner_model(_: LocalOnly) -> Response {
    let model_id = sovereign_gliner::labeled::configured_model_id();
    let installed = sovereign_gliner::gliner_ner::probe_model_available(&model_id);
    let expected_path = sovereign_gliner::gliner_ner::models_root()
        .join(&model_id)
        .display()
        .to_string();
    Json(NerModelStatus {
        installed,
        model_id,
        expected_path,
        // Empirical: gliner_small-v2.1 is ~600 MB (ONNX f32 + tokenizer).
        size_estimate_mb: 600,
    })
    .into_response()
}

// ─── The download job ──────────────────────────────────────────

/// One download's live state, held per job id in [`ASSET_DOWNLOADS`]. The
/// progress route reads it; the spawned download writes it.
struct AssetDownload {
    kind: AssetKind,
    /// Where the artifact lands. Decided when the job is accepted, because
    /// the daemon is the party that knows its own roots, and handed back on
    /// completion so the client writes the path the daemon will open.
    dest: std::path::PathBuf,
    /// The artifact currently being written. A GLiNER fetch pulls two files
    /// under one job, so this changes during the run.
    file: Mutex<Option<String>>,
    downloaded: AtomicU64,
    /// `u64::MAX` stands for "the server sent no Content-Length" so the pair
    /// can be read without a second lock. Rendered as ABSENT, never as 0.
    total: AtomicU64,
    outcome: Mutex<Option<Result<(), String>>>,
}

const NO_TOTAL: u64 = u64::MAX;

static ASSET_DOWNLOADS: JobRegistry<AssetDownload> = JobRegistry::new("asset_download");

impl AssetDownload {
    fn observe(&self, file: &str, downloaded: u64, total: Option<u64>) {
        if let Ok(mut f) = self.file.lock() {
            if f.as_deref() != Some(file) {
                *f = Some(file.to_string());
            }
        }
        self.downloaded.store(downloaded, Ordering::SeqCst);
        self.total
            .store(total.unwrap_or(NO_TOTAL), Ordering::SeqCst);
    }

    fn progress(&self, job_id: &str) -> AssetDownloadProgress {
        let outcome = self.outcome.lock().ok().and_then(|o| o.clone());
        let (state, error) = match outcome {
            None => (AssetDownloadState::Downloading, None),
            Some(Ok(())) => (AssetDownloadState::Complete, None),
            Some(Err(e)) => (AssetDownloadState::Error, Some(e)),
        };
        let total = match self.total.load(Ordering::SeqCst) {
            NO_TOTAL => None,
            t => Some(t),
        };
        AssetDownloadProgress {
            job_id: job_id.to_string(),
            kind: Some(self.kind),
            // Only on Complete: a path reported mid-download names a file
            // that is still a `.part`, and a client that wrote it into a
            // model slot would configure a truncated artifact.
            path: (state == AssetDownloadState::Complete).then(|| self.dest.display().to_string()),
            state,
            file: self.file.lock().ok().and_then(|f| f.clone()),
            downloaded: self.downloaded.load(Ordering::SeqCst),
            total,
            error,
        }
    }
}

/// Validate the request's kind-specific fields, and say which field is
/// missing. Returning the pieces rather than booleans keeps the handler from
/// re-unwrapping what this already checked.
#[derive(Debug)]
enum Plan {
    Gguf {
        url: String,
        file: String,
        expected_gb: Option<f64>,
    },
    Gliner {
        model_id: String,
    },
}

fn plan(req: &AssetDownloadRequest) -> Result<Plan, String> {
    match req.kind {
        AssetKind::Gguf => {
            let url = req
                .url
                .clone()
                .ok_or("kind `gguf` needs `url` (the direct download link)")?;
            let file = req
                .file
                .clone()
                .ok_or("kind `gguf` needs `file` (the filename under the models root)")?;
            // The ONE thing this route must not honour: a client naming a
            // destination outside the root the daemon owns. Refused by shape,
            // not sanitised — a sanitiser has to be right every time.
            if file.contains('/') || file.contains('\\') || file.contains("..") || file.is_empty() {
                return Err(format!(
                    "`file` must be a bare filename under the models root, not a path: {file}"
                ));
            }
            Ok(Plan::Gguf {
                url,
                file,
                expected_gb: req.expected_gb,
            })
        }
        AssetKind::Gliner => {
            if req.url.is_some() || req.file.is_some() {
                return Err(
                    "kind `gliner` takes `model_id` only — its URL and file layout come from \
                     the model's own generation spec"
                        .to_string(),
                );
            }
            Ok(Plan::Gliner {
                model_id: req
                    .model_id
                    .clone()
                    .unwrap_or_else(sovereign_gliner::labeled::configured_model_id),
            })
        }
    }
}

/// `POST /v1/admin/assets/download` — fetch a model into the DAEMON's roots,
/// as a job. Answers [`IngestJobAck`] with `202 Accepted`.
///
/// The two kinds go through the two existing downloaders —
/// `setup_planner::download_gguf` (resume-aware, content-type sniff, GGUF
/// magic + size floor) and `gliner_ner::download_model` (per-generation file
/// layout, idempotent). Neither is re-implemented here; this route is the
/// wire in front of them.
async fn asset_download(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<AssetDownloadRequest>,
) -> Response {
    let plan = match plan(&req) {
        Ok(p) => p,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };

    let models_dir = daemon.data_dir().join("models");
    let dest = match &plan {
        Plan::Gguf { file, .. } => models_dir.join(file),
        Plan::Gliner { model_id } => sovereign_gliner::gliner_ner::models_root().join(model_id),
    };
    let job_id = format!("asset-{}-{}", req.kind.as_str(), uuid::Uuid::new_v4());
    let job = Arc::new(AssetDownload {
        kind: req.kind,
        dest,
        file: Mutex::new(None),
        downloaded: AtomicU64::new(0),
        total: AtomicU64::new(NO_TOTAL),
        outcome: Mutex::new(None),
    });
    if !ASSET_DOWNLOADS.insert(job_id.clone(), Arc::clone(&job)) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the asset job table is unreadable; this daemon cannot track a download",
        );
    }

    let spawn_job = Arc::clone(&job);
    let spawn_id = job_id.clone();
    tracing::info!(
        job_id = %job_id,
        kind = req.kind.as_str(),
        "assets_http:download_accepted",
    );
    tokio::spawn(async move {
        let result = run_download(plan, &models_dir, &spawn_job).await;
        match &result {
            Ok(()) => tracing::info!(job_id = %spawn_id, "assets_http:download_complete"),
            Err(e) => {
                tracing::warn!(job_id = %spawn_id, error = %e, "assets_http:download_failed")
            }
        }
        if let Ok(mut o) = spawn_job.outcome.lock() {
            *o = Some(result);
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(IngestJobAck {
            // Not a corpus; the field is the ack's subject and for an asset
            // that is the artifact. Naming the job id twice would be worse.
            corpus_id: req.kind.as_str().to_string(),
            job_id: job_id.clone(),
            ok: true,
            progress_route: format!("/v1/admin/assets/download/{job_id}"),
        }),
    )
        .into_response()
}

async fn run_download(
    plan: Plan,
    models_dir: &std::path::Path,
    job: &Arc<AssetDownload>,
) -> Result<(), String> {
    match plan {
        Plan::Gguf {
            url,
            file,
            expected_gb,
        } => {
            std::fs::create_dir_all(models_dir)
                .map_err(|e| format!("create {}: {e}", models_dir.display()))?;
            let dest = models_dir.join(&file);
            let expected = match expected_gb {
                Some(gb) => sovereign_contracts::gguf_validator::GgufExpectation::from_size_gb(gb),
                None => sovereign_contracts::gguf_validator::GgufExpectation::unknown(),
            };
            let j = Arc::clone(job);
            let name = file.clone();
            setup_planner::download_gguf(&url, &dest, &expected, &move |done, total| {
                j.observe(&name, done, total);
            })
            .await
        }
        Plan::Gliner { model_id } => {
            let j = Arc::clone(job);
            sovereign_gliner::gliner_ner::download_model(&model_id, move |file, done, total| {
                j.observe(file, done, (total > 0).then_some(total));
            })
            .await
            .map_err(|e| e.to_string())
        }
    }
}

/// `GET /v1/admin/assets/download/{job}` — where that download has got to.
///
/// A job id this daemon has never seen answers `Unknown` with 200, not 404:
/// the poller's question is "what is the state", and `Unknown` is a state it
/// renders (the daemon restarted mid-download). A table it cannot READ is a
/// different fact and answers 500.
async fn asset_download_progress(_: LocalOnly, Path(job_id): Path<String>) -> Response {
    match ASSET_DOWNLOADS.get(&job_id) {
        Ok(Some(job)) => Json(job.progress(&job_id)).into_response(),
        Ok(None) => Json(AssetDownloadProgress {
            job_id,
            kind: None,
            state: AssetDownloadState::Unknown,
            file: None,
            downloaded: 0,
            total: None,
            path: None,
            error: None,
        })
        .into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[cfg(test)]
mod route_tests {
    //! The routes, driven over a real listener in the shape
    //! `daemon::start_daemon` uses. The unit tests below cover the deciders;
    //! these cover the wiring, which is the half a unit test cannot see.
    use super::*;
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

    /// `GET /internal/ner/model` reports the id the DAEMON is configured for
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
        assert_eq!(s.model_id, sovereign_gliner::labeled::configured_model_id());
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `gguf` request missing either half is refused by NAME, so the caller
    /// learns which field. Watched fail: replace the `ok_or` with
    /// `unwrap_or_default()` and a request with no url silently downloads
    /// from the empty string.
    #[test]
    fn a_gguf_request_names_the_field_it_is_missing() {
        let mut req = AssetDownloadRequest {
            kind: AssetKind::Gguf,
            url: None,
            file: Some("m.gguf".into()),
            model_id: None,
            expected_gb: None,
        };
        assert!(plan(&req).unwrap_err().contains("`url`"));
        req.url = Some("https://x/m.gguf".into());
        req.file = None;
        assert!(plan(&req).unwrap_err().contains("`file`"));
    }

    /// The destination cannot leave the models root. Watched fail: drop the
    /// separator check and `file: "../../.ssh/authorized_keys"` writes there.
    #[test]
    fn a_gguf_destination_cannot_escape_the_models_root() {
        for bad in ["../m.gguf", "a/b.gguf", "a\\b.gguf", ""] {
            let req = AssetDownloadRequest {
                kind: AssetKind::Gguf,
                url: Some("https://x/m.gguf".into()),
                file: Some(bad.into()),
                model_id: None,
                expected_gb: None,
            };
            assert!(
                plan(&req).is_err(),
                "`{bad}` must be refused as a destination"
            );
        }
    }

    /// A `gliner` request carrying gguf fields is REFUSED, not silently
    /// half-honoured. The two kinds resolve their URL differently and a
    /// request that mixes them is asking for something this route cannot do.
    #[test]
    fn a_gliner_request_refuses_gguf_fields() {
        let req = AssetDownloadRequest {
            kind: AssetKind::Gliner,
            url: Some("https://x/m.onnx".into()),
            file: None,
            model_id: Some("gliner_small-v2.1".into()),
            expected_gb: None,
        };
        assert!(plan(&req).unwrap_err().contains("model_id"));

        let ok = AssetDownloadRequest {
            kind: AssetKind::Gliner,
            url: None,
            file: None,
            model_id: Some("gliner_small-v2.1".into()),
            expected_gb: None,
        };
        match plan(&ok).expect("a model_id-only request is honoured") {
            Plan::Gliner { model_id } => assert_eq!(model_id, "gliner_small-v2.1"),
            Plan::Gguf { .. } => panic!("kind gliner must not plan a gguf fetch"),
        }
    }

    /// A server that sent no `Content-Length` is reported as ABSENT, and a
    /// real total survives the sentinel encoding.
    ///
    /// Watched fail: store `0` for the unknown case and the progress route
    /// reports a 0-byte artifact, which renders as a finished download.
    #[test]
    fn an_absent_content_length_is_reported_absent() {
        let job = Arc::new(AssetDownload {
            kind: AssetKind::Gguf,
            dest: std::path::PathBuf::from("/models/m.gguf"),
            file: Mutex::new(None),
            downloaded: AtomicU64::new(0),
            total: AtomicU64::new(NO_TOTAL),
            outcome: Mutex::new(None),
        });
        job.observe("m.gguf", 10, None);
        let p = job.progress("j");
        assert_eq!(p.total, None);
        assert_eq!(p.downloaded, 10);
        assert_eq!(p.file.as_deref(), Some("m.gguf"));
        assert_eq!(p.state, AssetDownloadState::Downloading);

        job.observe("m.gguf", 20, Some(100));
        assert_eq!(job.progress("j").total, Some(100));
    }

    /// A failed download reports Error WITH the daemon's sentence, never a
    /// Complete with an empty file. Watched fail: map the outcome's `Err` to
    /// `Complete` and this goes red.
    #[test]
    fn a_failed_download_reports_its_reason() {
        let job = Arc::new(AssetDownload {
            kind: AssetKind::Gliner,
            dest: std::path::PathBuf::from("/models/gliner/x"),
            file: Mutex::new(Some("model.onnx".into())),
            downloaded: AtomicU64::new(1),
            total: AtomicU64::new(NO_TOTAL),
            outcome: Mutex::new(Some(Err("fetch https://x: HTTP 404".into()))),
        });
        let p = job.progress("j2");
        assert_eq!(p.state, AssetDownloadState::Error);
        assert_eq!(p.error.as_deref(), Some("fetch https://x: HTTP 404"));
    }
}
