//! Tauri command surface for local corpora (Folder Drop + Obsidian).
//!
//! One job_id-scoped event channel per invocation:
//! `local-corpus://progress/{job_id}`. The UI listens with
//! `listen<LocalCorpusProgress>(channel, handler)`.
//!
//! Commands are thin — they translate TS-friendly shapes into
//! `LocalCorpusManager` calls and forward progress events via
//! `AppHandle::emit`. All heavy lifting happens in `sovereign-tools`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use sovereign_tools::local_corpus::{
    clusterer::ClusterConfig,
    git::GitStatus,
    manager::{IncompleteJob, ProgressCallback},
    pre_scanner::PreScanResult,
    preview::VaultPreview,
    progress::LocalCorpusProgress,
    writeback::{CleanResult, RollbackResult, SnapshotMeta, WriteBackResult},
    LocalCorpusConfig, LocalCorpusManager,
};
use std::path::PathBuf as StdPathBuf;

use crate::state::AppState;

// ─── Channel helpers ─────────────────────────────────────────────────

fn progress_channel(job_id: &str) -> String {
    format!("local-corpus://progress/{job_id}")
}

/// Build a progress callback that emits every `LocalCorpusProgress`
/// event on the job-scoped Tauri channel. `_ = emit(...)` because
/// a failed emit (window closed, e.g.) should not abort the long
/// running ingest — UI re-subscription will catch the terminal event
/// via the ingest result.
fn make_emitter(app: AppHandle, job_id: String) -> ProgressCallback {
    let channel = progress_channel(&job_id);
    Arc::new(move |evt: LocalCorpusProgress| {
        let _ = app.emit(&channel, &evt);
    })
}

fn new_job_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── Shared guards ───────────────────────────────────────────────────

async fn require_manager(
    state: &State<'_, Arc<AppState>>,
) -> Result<Arc<LocalCorpusManager>, String> {
    state
        .local_corpus
        .read()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            "Local corpus manager not ready. Finish setup (model + embedding model) first."
                .to_string()
        })
}

// ─── Command: lc_validate_path ───────────────────────────────────────

#[derive(Serialize)]
pub struct PathValidation {
    pub exists: bool,
    pub is_dir: bool,
    pub readable: bool,
    pub canonical_path: Option<String>,
}

/// Validate a user-supplied path. Returns readable metadata without
/// registering anything. Used by both the "Browse..." file dialog and
/// the file-drop handler before prompting confirmation.
#[tauri::command]
pub async fn lc_validate_path(path: String) -> Result<PathValidation, String> {
    let p = PathBuf::from(&path);
    let exists = p.exists();
    let is_dir = p.is_dir();
    let readable = p
        .metadata()
        .and_then(|_| std::fs::read_dir(&p))
        .is_ok();
    let canonical_path = p
        .canonicalize()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    Ok(PathValidation {
        exists,
        is_dir,
        readable,
        canonical_path,
    })
}

// ─── Command: lc_pre_scan ────────────────────────────────────────────

#[derive(Serialize)]
pub struct PreScanResponse {
    pub job_id: String,
    pub result: PreScanResult,
    pub corpus_id: String,
    pub display_name: String,
}

/// Register (or re-register) a corpus for the supplied path + source
/// type, then run a pre-scan. Returns the classification and the new
/// corpus_id. Progress events are emitted on
/// `local-corpus://progress/{job_id}` but the command is synchronous
/// end-to-end — callers await the return value.
///
/// `source_type` is `"obsidian"` or `"folder"`. `display_name` defaults
/// to the folder's basename.
#[tauri::command]
pub async fn lc_pre_scan(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    path: String,
    source_type: String,
    display_name: Option<String>,
) -> Result<PreScanResponse, String> {
    let manager = require_manager(&state).await?;
    let p = PathBuf::from(&path);
    if !p.exists() || !p.is_dir() {
        return Err(format!("Path does not exist or is not a directory: {path}"));
    }

    let config = match source_type.as_str() {
        "obsidian" => {
            let snap = manager.snapshot_root().to_path_buf();
            LocalCorpusConfig::obsidian_vault(p, snap)
        }
        "folder" => {
            let name = display_name.unwrap_or_else(|| {
                std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Documents")
                    .to_string()
            });
            LocalCorpusConfig::document_folder(PathBuf::from(&path), name)
        }
        other => return Err(format!("Unknown source_type: {other}")),
    };

    let job_id = new_job_id();
    let progress = Some(make_emitter(app.clone(), job_id.clone()));
    let corpus_id = manager
        .register(config.clone())
        .await
        .map_err(|e| format!("register: {e}"))?;

    let result = manager
        .pre_scan(&corpus_id, progress)
        .await
        .map_err(|e| format!("pre_scan: {e}"))?;

    Ok(PreScanResponse {
        job_id,
        result,
        corpus_id,
        display_name: config.display_name,
    })
}

// ─── Command: lc_ingest ──────────────────────────────────────────────

/// Begin ingestion for an already-registered corpus. Returns a
/// `job_id` immediately; callers listen on
/// `local-corpus://progress/{job_id}` for phase events and the
/// terminal `Complete { result: Ingest(stats) }` payload.
///
/// Ingestion runs in a spawned task so the command itself can return
/// promptly — the UI progress panel is driven entirely by events.
#[tauri::command]
pub async fn lc_ingest(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<String, String> {
    let manager = require_manager(&state).await?;
    let job_id = new_job_id();
    let progress = make_emitter(app.clone(), job_id.clone());

    // Spawn so the command doesn't block. The UI drives the progress
    // panel off the emit channel; failure propagates via
    // `LocalCorpusProgress::Error`.
    tokio::spawn(async move {
        match manager.ingest(&corpus_id, Some(progress.clone())).await {
            Ok(_stats) => {
                // The manager already emits Complete; nothing to do
                // here.
            }
            Err(e) => {
                let err = LocalCorpusProgress::Error {
                    message: e.to_string(),
                    recoverable: false,
                };
                progress(err);
            }
        }
    });
    Ok(job_id)
}

// ─── Command: lc_list ────────────────────────────────────────────────

#[tauri::command]
pub async fn lc_list(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LocalCorpusConfig>, String> {
    let manager = require_manager(&state).await?;
    Ok(manager.list().await)
}

// ─── Command: lc_remove ──────────────────────────────────────────────

#[tauri::command]
pub async fn lc_remove(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let manager = require_manager(&state).await?;
    manager
        .remove(&corpus_id)
        .await
        .map_err(|e| format!("remove: {e}"))
}

// ─── Command: lc_incomplete_jobs ────────────────────────────────────

#[tauri::command]
pub async fn lc_incomplete_jobs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<IncompleteJob>, String> {
    let manager = require_manager(&state).await?;
    Ok(manager.incomplete_jobs().await)
}

// ─── Command: lc_cancel ──────────────────────────────────────────────

/// Signal a running ingest (or cluster) for `corpus_id` to stop
/// cooperatively. Returns `true` when a flag was found and flipped.
/// The progress channel emits its final `Error { recoverable: true }`
/// once the engine loop exits.
#[tauri::command]
pub async fn lc_cancel(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<bool, String> {
    let manager = require_manager(&state).await?;
    Ok(manager.cancel(&corpus_id))
}

// ─── Command: lc_check_git ───────────────────────────────────────────

#[tauri::command]
pub async fn lc_check_git(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<GitStatus>, String> {
    let manager = require_manager(&state).await?;
    manager
        .check_git(&corpus_id)
        .await
        .map_err(|e| format!("check_git: {e}"))
}

// ─── Command: lc_write_tags ──────────────────────────────────────────

#[tauri::command]
pub async fn lc_write_tags(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    git_commit: Option<bool>,
) -> Result<WriteBackResult, String> {
    let manager = require_manager(&state).await?;
    manager
        .write_tags(&corpus_id, git_commit.unwrap_or(false))
        .await
        .map_err(|e| format!("write_tags: {e}"))
}

// ─── Command: lc_list_snapshots ──────────────────────────────────────

#[tauri::command]
pub async fn lc_list_snapshots(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Vec<SnapshotMeta>, String> {
    let manager = require_manager(&state).await?;
    manager
        .list_snapshots(&corpus_id)
        .await
        .map_err(|e| format!("list_snapshots: {e}"))
}

// ─── Command: lc_rollback ────────────────────────────────────────────

#[tauri::command]
pub async fn lc_rollback(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    snapshot_path: String,
) -> Result<RollbackResult, String> {
    let manager = require_manager(&state).await?;
    let path = StdPathBuf::from(snapshot_path);
    manager
        .rollback(&corpus_id, &path)
        .await
        .map_err(|e| format!("rollback: {e}"))
}

// ─── Command: lc_clean ───────────────────────────────────────────────

#[tauri::command]
pub async fn lc_clean(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<CleanResult, String> {
    let manager = require_manager(&state).await?;
    manager
        .clean(&corpus_id)
        .await
        .map_err(|e| format!("clean: {e}"))
}

// ─── Command: lc_search ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct LocalSearchHit {
    pub content: String,
    pub title: Option<String>,
    pub corpus_id: String,
    pub score: f32,
}

// ─── Command: lc_cluster ─────────────────────────────────────────────

/// Begin clustering + LLM labelling for an already-ingested Obsidian
/// vault. Returns a `job_id` immediately; caller subscribes to the
/// progress channel as with ingestion.
#[tauri::command]
pub async fn lc_cluster(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    config: Option<ClusterConfig>,
) -> Result<String, String> {
    let manager = require_manager(&state).await?;
    let cfg = config.unwrap_or_default();
    let job_id = new_job_id();
    let progress = make_emitter(app.clone(), job_id.clone());

    tokio::spawn(async move {
        match manager.cluster(&corpus_id, &cfg, progress.clone()).await {
            Ok(_) => {
                // Emit a terminal Complete event. The UI calls
                // `lc_get_preview` next to fetch the renderable
                // shape; we don't inline it here because the preview
                // blob can be large (per-note assignments) and
                // progress events are meant to be cheap.
                progress(LocalCorpusProgress::Complete {
                    result: sovereign_tools::local_corpus::progress::CompletionResult::Ingest(
                        sovereign_tools::local_corpus::manager::IngestStats {
                            corpus_id: corpus_id.clone(),
                            files_indexed: 0,
                            chunks_written: 0,
                            runtime_failures: Vec::new(),
                            excerpt_chunks: Vec::new(),
                            duration_secs: 0,
                        },
                    ),
                });
            }
            Err(e) => {
                progress(LocalCorpusProgress::Error {
                    message: e.to_string(),
                    recoverable: false,
                });
            }
        }
    });
    Ok(job_id)
}

// ─── Command: lc_get_preview ─────────────────────────────────────────

/// Fetch the computed preview for a corpus that has had `lc_cluster`
/// run recently. Returns `NotFound` if no cluster result is cached.
#[tauri::command]
pub async fn lc_get_preview(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    config: Option<ClusterConfig>,
) -> Result<VaultPreview, String> {
    let manager = require_manager(&state).await?;
    let cfg = config.unwrap_or_default();
    manager
        .get_preview(&corpus_id, &cfg)
        .await
        .map_err(|e| format!("get_preview: {e}"))
}

// ─── Command: lc_search ─────────────────────────────────────────────

#[tauri::command]
pub async fn lc_search(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<LocalSearchHit>, String> {
    let manager = require_manager(&state).await?;
    let hits = manager
        .search(&corpus_id, &query, limit.unwrap_or(10))
        .await
        .map_err(|e| format!("search: {e}"))?;
    Ok(hits
        .into_iter()
        .map(|c| LocalSearchHit {
            content: c.content,
            title: c.title,
            corpus_id: c.corpus_id,
            score: c.score,
        })
        .collect())
}

