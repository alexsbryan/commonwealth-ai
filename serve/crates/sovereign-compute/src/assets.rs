// SPDX-License-Identifier: AGPL-3.0-or-later
//! serve's weights: whether the NER model is installed, and the one job that
//! fetches a model into the serving root. Moved from the svrn daemon's
//! assets_http.rs (pb-serve-distributes, rung 1: serve's weights), so the
//! process that loads a model is the one that writes it: `serve` mounts
//! [`bundle`], and the daemon forwards its routes to serve.
//!
//! Anything that downloads is a job in the IndexBuild pattern (202 + a
//! progress route); a client never awaits a 600 MB fetch on a request thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use host_kit::jobs::JobRegistry;
use host_kit::shell::guard::{loopback_only, LocalOnly};
use host_kit::shell::RouteBundle;
use sovereign_contracts::daemon_wire::{
    AssetDownloadProgress, AssetDownloadRequest, AssetDownloadState, AssetKind, IngestJobAck,
    NerModelStatus,
};
use sovereign_inference::setup_planner;

use crate::setup_reads::json_error;

/// The NER read and the download job as `serve`'s named bundle, at the paths
/// the daemon serves them on, loopback-only: they write into the serving root
/// and name paths on this disk. `models_dir` is where a GGUF lands
/// (`<data dir>/models`).
pub fn bundle(models_dir: PathBuf) -> RouteBundle {
    let guard = || axum::middleware::from_fn(loopback_only);
    RouteBundle::new("assets")
        .route("/internal/ner/model", get(ner_model).layer(guard()))
        .route(
            "/v1/admin/assets/download",
            post(asset_download).layer(guard()),
        )
        .route(
            "/v1/admin/assets/download/{job}",
            get(asset_download_progress).layer(guard()),
        )
        .with_state(Arc::new(models_dir))
}

/// `GET /internal/ner/model` — is the entity extractor's model installed
/// where the extractor would load it.
///
/// `model_id` is the serving process's `configured_model_id()`, not a
/// constant the client repeats: a host with `SOVEREIGN_GLINER_MODEL_ID` set
/// loads a different export, and a client hardcoding the default would report
/// "installed" about a file the extractor never opens.
async fn ner_model(_: LocalOnly) -> Response {
    ner_model_status()
}

/// The answer of `GET /internal/ner/model`, for a host that serves the route
/// in process.
pub fn ner_model_status() -> Response {
    let model_id = crate::ner::configured_model_id();
    let installed = crate::ner::probe_model_available(&model_id);
    let expected_path = crate::ner::models_root()
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
                    .unwrap_or_else(crate::ner::configured_model_id),
            })
        }
    }
}

/// `POST /v1/admin/assets/download` — fetch a model into the SERVING roots,
/// as a job. Answers [`IngestJobAck`] with `202 Accepted`.
///
/// The two kinds go through the two existing downloaders —
/// `setup_planner::download_gguf` (resume-aware, content-type sniff, GGUF
/// magic + size floor) and `gliner_ner::download_model` (per-generation file
/// layout, idempotent). Neither is re-implemented here; this route is the
/// wire in front of them.
async fn asset_download(
    _: LocalOnly,
    State(models_dir): State<Arc<PathBuf>>,
    Json(req): Json<AssetDownloadRequest>,
) -> Response {
    download(models_dir.as_ref().clone(), req).await
}

/// The answer of `POST /v1/admin/assets/download` into `models_dir`, for a
/// host that serves the route in process.
pub async fn download(models_dir: PathBuf, req: AssetDownloadRequest) -> Response {
    let plan = match plan(&req) {
        Ok(p) => p,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };

    let dest = match &plan {
        Plan::Gguf { file, .. } => models_dir.join(file),
        Plan::Gliner { model_id } => crate::ner::models_root().join(model_id),
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
            "the asset job table is unreadable; this process cannot track a download",
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
            crate::ner::download_model(&model_id, move |file, done, total| {
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
    download_progress(job_id)
}

/// The answer of `GET /v1/admin/assets/download/{job}`, for a host that
/// serves the route in process.
pub fn download_progress(job_id: String) -> Response {
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
#[path = "assets/route_tests.rs"]
mod route_tests;
#[cfg(test)]
#[path = "assets/unit_tests.rs"]
mod unit_tests;
