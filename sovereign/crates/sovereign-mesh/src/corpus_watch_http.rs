// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP routes for the watched-folder reconciliation subsystem.
//!
//! Mounted onto the daemon's loopback-only client router via
//! `EmbeddedDaemon::install_corpus_watch_http_router`. The handlers
//! reach into the `watched_folder_runtime` singleton for the
//! manager + registry — same pattern `watched_folder_runtime`
//! describes.
//!
//! Routes (all under `/internal/corpus/watch/`):
//!
//! | Method | Path                           | Purpose                                   |
//! |--------|--------------------------------|-------------------------------------------|
//! | POST   | `/register`                    | Register a new watched-folder corpus      |
//! | GET    | `/list`                        | List every registered watched-folder      |
//! | GET    | `/status/{corpus_id}`          | Status DTO for one corpus                 |
//! | POST   | `/pause/{corpus_id}`           | Pause sweeps (manual)                     |
//! | POST   | `/resume/{corpus_id}`          | Resume after manual pause                 |
//! | POST   | `/confirm-deletion/{corpus_id}`| Acknowledge guard-tripped pause           |
//! | DELETE | `/{corpus_id}`                 | Unregister + remove index                 |
//!
//! All responses are JSON. Error shape: `{ "error": "<message>" }`.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::extract::{ConnectInfo, Json, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use sovereign_tools::local_corpus::config::{LocalCorpusConfig, WatchedFolderConfig};
use sovereign_tools::local_corpus::watched::state::FailedFile;
use sovereign_tools::local_corpus::watched::status::WatchedFolderStatus;
use sovereign_tools::local_corpus::WatchedIncompleteJob;

use crate::loopback_guard::enforce_localhost;
use crate::watched_folder_runtime;

/// Build the watched-folder router. Mounts under
/// `/internal/corpus/watch/...` and applies the loopback-only guard
/// per the rest of the internal route surface.
pub fn corpus_watch_router() -> Router {
    Router::new()
        .route("/internal/corpus/watch/register", post(register_handler))
        .route("/internal/corpus/watch/list", get(list_handler))
        .route(
            "/internal/corpus/watch/incomplete-jobs",
            get(incomplete_jobs_handler),
        )
        .route(
            "/internal/corpus/watch/status/{corpus_id}",
            get(status_handler),
        )
        .route(
            "/internal/corpus/watch/state/{corpus_id}",
            get(state_handler),
        )
        .route(
            "/internal/corpus/watch/details/{corpus_id}",
            get(details_handler),
        )
        .route(
            "/internal/corpus/watch/document/{corpus_id}/{doc_id}",
            get(document_handler),
        )
        .route(
            "/internal/corpus/watch/pause/{corpus_id}",
            post(pause_handler),
        )
        .route(
            "/internal/corpus/watch/resume/{corpus_id}",
            post(resume_handler),
        )
        .route(
            "/internal/corpus/watch/confirm-deletion/{corpus_id}",
            post(confirm_deletion_handler),
        )
        .route(
            "/internal/corpus/watch/sync-now/{corpus_id}",
            post(sync_now_handler),
        )
        .route(
            "/internal/corpus/watch/{corpus_id}/roots",
            post(add_root_handler),
        )
        .route(
            "/internal/corpus/watch/{corpus_id}/roots/{idx}",
            delete(remove_root_handler),
        )
        .route(
            "/internal/corpus/watch/{corpus_id}/enrich/enable",
            post(enrich_enable_handler),
        )
        .route(
            "/internal/corpus/watch/{corpus_id}/enrich/disable",
            post(enrich_disable_handler),
        )
        .route(
            "/internal/corpus/watch/{corpus_id}/enrich/rebuild",
            post(enrich_rebuild_handler),
        )
        // Re-enrich a SINGLE note (the "flag a wrong summary" revision
        // loop). The correction ledger row is written desktop-side first;
        // this just re-runs that one note's RAPTOR with the hint applied.
        .route(
            "/internal/corpus/watch/{corpus_id}/enrich/reenrich-note",
            post(reenrich_note_handler),
        )
        // One-shot enrichment for a corpus the *desktop* ingested with its own
        // provider-less manager — e.g. a drag-drop DocumentFolder. Registers it
        // into the daemon's tiered-capable manager (no sweep worker) and runs a
        // single tiered build. "Watched-folder registration without the watcher."
        .route("/internal/corpus/enrich-once", post(enrich_once_handler))
        // Clear a "zombie" enrichment / watched-folder status (a stalled or
        // crashed build stuck at "Preparing to build the map", or a sticky
        // Errored sweep) so the corpus can be rebuilt / swept again.
        .route("/internal/corpus/enrich-reset", post(enrich_reset_handler))
        .route("/internal/corpus/watch/{corpus_id}", delete(remove_handler))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
}

// ─── Wire types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterRequest {
    /// Absolute path to the folder to watch. Must exist on disk.
    pub path: PathBuf,
    /// Optional human-readable display name. Defaults to the
    /// folder's basename.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Per-corpus configuration; merged into `WatchedFolderConfig`'s
    /// defaults via serde defaults on each field.
    #[serde(default)]
    pub config: WatchedFolderConfig,
    /// When true, the daemon kicks off the initial sweep
    /// synchronously before responding. When false (the default),
    /// the corpus is registered and the next scheduler tick picks
    /// it up — keeps the HTTP request fast for big folders.
    #[serde(default)]
    pub sync_initial: bool,
}

// ─── Response types ──────────────────────────────────────────────
//
// `Deserialize` is not dead weight: sovereign-desktop imports these as its
// HTTP CLIENT types (`watched_folder_commands`), so this module is the one
// definition of the `/internal/corpus/watch/*` contract for both ends. It
// derived `Serialize` alone until 2026-08-21, which is why the desktop
// carried seven hand-copied mirrors that had silently drifted. Do not drop
// it, and add fields here rather than in a client.

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub corpus_id: String,
    pub display_name: String,
    pub initial_sweep: InitialSweepStatus,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InitialSweepStatus {
    Skipped,
    Spawned {
        corpus_id: String,
    },
    Completed {
        files_indexed: usize,
        chunks_written: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListResponse {
    pub corpora: Vec<ListEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListEntry {
    pub corpus_id: String,
    pub display_name: String,
    pub root_path: PathBuf,
    pub status: WatchedFolderStatus,
    /// Folder-ingest v1 §3.5. `"continuous"` or `"manual"`.
    ///
    /// `serde(default)` on this and the two fields below is for the CLIENT
    /// direction only: in Attach mode the desktop may be talking to an older
    /// `sovereign daemon` that predates folder-ingest v1 and omits them.
    /// Without it the whole watched-folder list fails to parse rather than
    /// degrading to the pre-v1 defaults.
    #[serde(default)]
    pub sync_mode: sovereign_tools::local_corpus::config::SyncMode,
    /// Folder-ingest v1 §3.4. When `true`, the folder is excluded
    /// from ambient situated-context assembly. UI surfaces a badge.
    #[serde(default)]
    pub sensitive: bool,
    /// Folder-ingest v1 §3.1. Number of additional roots layered
    /// on top of the primary; the card UI surfaces "+N folders"
    /// when non-zero so a multi-root corpus is identifiable
    /// without opening the detail panel.
    #[serde(default)]
    pub additional_roots_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub corpus_id: String,
    pub status: WatchedFolderStatus,
}

/// Richer status response — includes the per-extension skipped
/// breakdown and the failed-file detail. Returned from the
/// `/state/{corpus_id}` route. Kept separate from `StatusResponse`
/// so a polling caller doesn't pay for the larger payload on every
/// tick.
#[derive(Debug, Serialize, Deserialize)]
pub struct StateResponse {
    pub corpus_id: String,
    pub status: WatchedFolderStatus,
    pub skipped_by_extension: std::collections::HashMap<String, usize>,
    pub failed_files: Vec<FailedFile>,
    pub tombstones: usize,
    pub live_entries: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IncompleteJobsResponse {
    pub jobs: Vec<WatchedIncompleteJob>,
}

/// Folder-ingest v1 §3.7 — glassbox folder-detail digest. Returned
/// from `GET /internal/corpus/watch/details/{corpus_id}`. Heavier
/// than `StateResponse`; the desktop fetches it once when the user
/// opens the detail panel, not on every poll tick.
#[derive(Debug, Serialize)]
pub struct DetailsResponse {
    pub corpus_id: String,
    pub display_name: String,
    pub root_path: PathBuf,
    pub status: WatchedFolderStatus,
    /// Sync cadence policy. Mirrors `WatchedFolderConfig.sync_mode`.
    pub sync_mode: sovereign_tools::local_corpus::config::SyncMode,
    /// `true` when the folder is excluded from ambient situated-
    /// context assembly. Mirrors `WatchedFolderConfig.sensitive`.
    pub sensitive: bool,
    /// Total live (indexed) documents.
    pub live_entries: usize,
    /// Per-extension count of indexed documents. Derived from the
    /// state's `entries` map at request time so the user sees the
    /// breakdown of what *is* indexed vs. the negative-space below.
    pub formats: std::collections::HashMap<String, usize>,
    /// Per-extension count of files the walker saw but couldn't
    /// dispatch to an extractor. Same shape as `state.skipped_by_extension`.
    pub skipped_by_extension: std::collections::HashMap<String, usize>,
    /// Failed extractions grouped by reason kind. The §3.7 "What I
    /// don't have" surface renders one row per group ("3 corrupt",
    /// "1 password-protected", …) with the per-file detail one
    /// click deeper. Each `FailedFile` carries the absolute path +
    /// reason string for inspection.
    pub failed_files: Vec<FailedFile>,
    /// Tombstone count — files removed within the soft-delete grace
    /// window that can still be revived by restoring the same
    /// content hash.
    pub tombstones: usize,
    /// Enrichment status. Phase E will populate the `Building` /
    /// `Complete` variants; Phase C ships with `Off` for every
    /// corpus so the UI surface lands ahead of the orchestration.
    pub enrichment: EnrichmentStatus,
    /// Last sweep start time (Unix seconds). 0 = no sweep run yet.
    /// Surfaced separately from `status` because the user wants to
    /// see "last synced 2m ago" regardless of whether the corpus is
    /// currently Idle, Sweeping, or Paused.
    pub last_sweep_unix: u64,
    /// Folder-ingest v1 §3.1 multi-root: every root attached to
    /// this corpus (primary first, then each additional in
    /// declared order). Always at least 1 entry.
    pub roots: Vec<RootEntry>,
}

/// One root attached to a watched-folder corpus, surfaced on
/// `DetailsResponse.roots`. `idx == 0` is the primary
/// `LocalCorpusConfig.root_path`; `idx >= 1` map onto
/// `WatchedFolderConfig.additional_roots[idx - 1]`. The UI uses
/// `idx` as the argument to `DELETE /watch/{c}/roots/{idx-1}`
/// for the remove-root button (subtracting 1 to land on the
/// additional_roots array index).
///
/// `doc_count` is derived from `state.entries` at request time —
/// the entries' `source_root_index` mirrors the same `idx` here.
#[derive(Debug, Serialize)]
pub struct RootEntry {
    pub idx: u8,
    pub path: PathBuf,
    /// Unix seconds when the root was attached. `0` for the
    /// primary root (registered at corpus creation; the corpus's
    /// `installed_at` already covers that timestamp elsewhere).
    pub added_at_unix: u64,
    pub doc_count: usize,
    /// `true` for the primary root only. The UI surfaces this so
    /// "Stop watching this folder" applies to the corpus while
    /// "Remove this root" applies only to additional roots.
    pub primary: bool,
}

/// Per-folder enrichment status. Folder-ingest v1 §3.3.
///
/// Phase E populates the full lifecycle. The wire shape mirrors
/// `WatchedFolderState.enrichment_status` so the UI can render
/// progress without a second round-trip.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnrichmentStatus {
    /// No atlas. Folder is searchable via standard retrieval.
    Off,
    /// Build is in flight. UI renders a phase + progress.
    Building {
        pipeline_id: String,
        phase: String,
        current: usize,
        total: usize,
        started_at_unix: u64,
    },
    /// Last build succeeded. UI offers Disable + Rebuild.
    Complete {
        pipeline_id: String,
        built_at_unix: u64,
        doc_count: usize,
        /// Live entry count at request time. UI computes "M new
        /// docs since last build" as `live_entries - doc_count`.
        current_doc_count: usize,
    },
    /// Last build failed (or was cancelled). UI surfaces the
    /// reason verbatim.
    Failed {
        pipeline_id: String,
        failed_at_unix: u64,
        reason: String,
    },
}

/// Folder-ingest v1 §3.7 — per-document inspection digest.
/// Returned from `GET /internal/corpus/watch/document/{c}/{d}`.
/// Renders into `DocumentInspector`.
#[derive(Debug, Serialize)]
pub struct DocumentResponse {
    pub corpus_id: String,
    pub doc_id: String,
    pub absolute_path: PathBuf,
    pub size_bytes: u64,
    /// File mtime (Unix seconds). Helps the user decide whether
    /// the indexed view is fresh.
    pub mtime_unix: i64,
    /// 16-char sha256 prefix from the walker fast-path. Mostly
    /// useful for cross-referencing the same content across
    /// multiple roots once Phase D lands; surfaced now so the
    /// document-inspector view doesn't churn at that point.
    pub content_hash: String,
    /// Number of chunks the engine has indexed for this document.
    /// Zero when the file failed extraction or the initial sweep
    /// hasn't reached it yet.
    pub chunk_count: usize,
    /// First chunk's content, truncated to ~500 chars. `None`
    /// when `chunk_count == 0`.
    pub first_chunk_preview: Option<String>,
    /// Atom contributions. Empty until Phase E lands per-folder
    /// enrichment; the type is wired ahead so the UI doesn't
    /// churn when it does.
    pub atoms: Vec<DocumentAtom>,
}

#[derive(Debug, Serialize)]
pub struct DocumentAtom {
    pub atom_id: String,
    pub atom_type: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PauseRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Folder-ingest v1 §3.1 — body for `POST /watch/{id}/roots`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddRootRequest {
    /// Absolute path to attach as an additional root. Must be a
    /// real, readable directory; rejected otherwise. Manager
    /// canonicalises before persistence.
    pub path: PathBuf,
}

/// Folder-ingest v1 §3.3 — body for `POST /watch/{id}/enrich/enable`.
#[derive(Debug, Clone, Deserialize)]
pub struct EnrichEnableRequest {
    /// Atlas pipeline id. One of `philosophy_atlas`,
    /// `referential_atlas`, `literary_atlas`. The CLI's
    /// `enrich build` will reject any other value at start.
    pub pipeline_id: String,
}

/// Folder-ingest v1 §3.3 — response from enable / rebuild. The
/// `job_id` is the internal handle the driver assigned; the UI
/// uses it to subscribe to progress events on
/// `enrich://progress/<job_id>`.
#[derive(Debug, Serialize)]
pub struct EnrichJobAck {
    pub corpus_id: String,
    pub job_id: String,
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AckResponse {
    pub corpus_id: String,
    pub ok: bool,
}

/// Response from `enrich-once`: the daemon ingested the folder (stats
/// below) and kicked off the tiered atlas build in the background. The
/// atlas status is pollable via `/internal/enrichment/status`.
#[derive(Debug, Serialize)]
pub struct EnrichOnceAck {
    pub corpus_id: String,
    pub files_indexed: usize,
    pub chunks_written: u64,
    pub ok: bool,
    /// True when the request was a no-op because a build for this corpus
    /// was already in flight (idempotency short-circuit). `files_indexed`
    /// / `chunks_written` are 0 in that case — the owning request reports
    /// the real counts. Surfaced for observability; the manual
    /// `lc_enrich_now` trigger that can race a build ignores the body and
    /// just polls `/internal/enrichment/status`.
    #[serde(default)]
    pub already_running: bool,
}

/// Body for `POST /internal/corpus/enrich-reset`.
#[derive(Debug, Deserialize)]
pub struct EnrichResetRequest {
    pub corpus_id: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

// ─── Handlers ────────────────────────────────────────────────────

async fn register_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<RegisterRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    let Some(registry) = watched_folder_runtime::registry() else {
        return service_unavailable("watched-folder registry not installed").into_response();
    };

    let display_name = req
        .display_name
        .clone()
        .unwrap_or_else(|| basename_or_unknown(&req.path));
    let cfg = LocalCorpusConfig::watched_folder(
        req.path.clone(),
        display_name.clone(),
        req.config.clone(),
    );
    let corpus_id = cfg.id.clone();
    let sweep_interval = req.config.sweep_interval_secs;
    let sync_mode = req.config.sync_mode;

    if let Err(e) = manager.register(cfg).await {
        return error(StatusCode::INTERNAL_SERVER_ERROR, format!("register: {e}")).into_response();
    }

    // Register in the scheduler's registry so the next tick picks
    // it up. Idempotent — re-registering refreshes the cadence
    // and sync_mode. Threading sync_mode through here is what makes
    // Manual-mode corpora actually opt out of periodic dispatch
    // — without it, the registry would default to Continuous and
    // sweep regardless of the user's choice.
    registry
        .register_with_mode(corpus_id.clone(), sweep_interval, sync_mode)
        .await;

    let initial = if req.sync_initial {
        match manager.ingest(&corpus_id, None, None).await {
            Ok(stats) => InitialSweepStatus::Completed {
                files_indexed: stats.files_indexed,
                chunks_written: stats.chunks_written,
            },
            Err(e) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("initial ingest: {e}"),
                )
                .into_response();
            }
        }
    } else {
        // Spawn the initial ingest detached. Failures surface in
        // tracing (and on the next sweep, which will retry the
        // missing files as adds).
        let manager_for_spawn = manager.clone();
        let id_for_spawn = corpus_id.clone();
        tokio::spawn(async move {
            if let Err(e) = manager_for_spawn.ingest(&id_for_spawn, None, None).await {
                tracing::warn!(
                    corpus_id = %id_for_spawn,
                    error = %e,
                    "watched_folder:initial_ingest_failed"
                );
            }
        });
        InitialSweepStatus::Spawned {
            corpus_id: corpus_id.clone(),
        }
    };

    Json(RegisterResponse {
        corpus_id,
        display_name,
        initial_sweep: initial,
    })
    .into_response()
}

async fn list_handler(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };

    // `list_reconcilable`, NOT `list_watched`: this endpoint answers
    // "what is the daemon keeping in sync?", and the scheduler seeds
    // itself from the reconcilable set (watched folders + obsidian
    // vaults) in `watched_folder_setup::WatchedSubsystem::install`.
    // Listing the narrower set meant an actively-swept vault never
    // appeared here — including one parked in a tripped deletion
    // guard for six days (2026-08-02). The listing surface and the
    // dispatch surface must read the same set or the operator view
    // is fiction.
    let corpora = manager.list_reconcilable().await;
    let mut entries = Vec::with_capacity(corpora.len());
    for cfg in corpora {
        let status = manager
            .watched_status(&cfg.id)
            .await
            .unwrap_or(WatchedFolderStatus::Idle {
                last_sweep_unix: 0,
                live_docs: 0,
                tombstones: 0,
            });
        let (sync_mode, sensitive, additional_roots_count) = cfg
            .source_type
            .watched_config()
            .map(|w| (w.sync_mode, w.sensitive, w.additional_roots.len()))
            .unwrap_or((
                sovereign_tools::local_corpus::config::SyncMode::Continuous,
                false,
                0,
            ));
        entries.push(ListEntry {
            corpus_id: cfg.id,
            display_name: cfg.display_name,
            root_path: cfg.root_path,
            status,
            sync_mode,
            sensitive,
            additional_roots_count,
        });
    }
    Json(ListResponse { corpora: entries }).into_response()
}

async fn status_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.watched_status(&corpus_id).await {
        Ok(status) => Json(StatusResponse { corpus_id, status }).into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, format!("{e}")).into_response(),
    }
}

async fn state_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.watched_state(&corpus_id).await {
        Ok(state) => Json(StateResponse {
            corpus_id,
            status: state.status,
            skipped_by_extension: state.skipped_by_extension,
            failed_files: state.failed_files,
            tombstones: state.tombstones.len(),
            live_entries: state.entries.len(),
        })
        .into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, format!("{e}")).into_response(),
    }
}

/// `GET /internal/corpus/watch/details/{corpus_id}` — the §3.7
/// glassbox folder-detail digest. Renders into `WatchedFolderDetail`.
/// Heavier than `state_handler`; not for high-frequency polling.
async fn details_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };

    // The config is the source of truth for display_name / root_path
    // / sync_mode / sensitive. State holds the live counts.
    let configs = manager.list_watched().await;
    let cfg = match configs.into_iter().find(|c| c.id == corpus_id) {
        Some(c) => c,
        None => {
            return error(
                StatusCode::NOT_FOUND,
                format!("corpus '{corpus_id}' not registered"),
            )
            .into_response();
        }
    };
    let (sync_mode, sensitive) = cfg
        .source_type
        .watched_config()
        .map(|w| (w.sync_mode, w.sensitive))
        .unwrap_or((
            sovereign_tools::local_corpus::config::SyncMode::Continuous,
            false,
        ));

    let state = match manager.watched_state(&corpus_id).await {
        Ok(s) => s,
        Err(e) => {
            return error(StatusCode::NOT_FOUND, format!("{e}")).into_response();
        }
    };

    // Per-extension index of *indexed* docs. The walker keys
    // entries on the relative doc_id, which is the source path —
    // so the extension comes off the key string. Files without an
    // extension bucket as `(no extension)` to mirror the same
    // labelling the skipped-by-extension breakdown uses.
    let mut formats: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for doc_id in state.entries.keys() {
        let ext = std::path::Path::new(doc_id)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| "(no extension)".to_string());
        *formats.entry(ext).or_insert(0) += 1;
    }

    // Folder-ingest v1 §3.1 — assemble the roots array. `doc_count`
    // is computed from `state.entries[*].source_root_index`, which
    // the walker stamps in `walk_one_root`.
    let mut docs_per_root: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();
    for entry in state.entries.values() {
        *docs_per_root.entry(entry.source_root_index).or_insert(0) += 1;
    }
    let watched_cfg_for_roots = cfg.source_type.watched_config();
    let mut roots: Vec<RootEntry> = Vec::new();
    roots.push(RootEntry {
        idx: 0,
        path: cfg.root_path.clone(),
        added_at_unix: 0,
        doc_count: docs_per_root.get(&0).copied().unwrap_or(0),
        primary: true,
    });
    if let Some(w) = watched_cfg_for_roots {
        for (i, spec) in w.additional_roots.iter().enumerate() {
            let root_idx = u8::try_from(i + 1).unwrap_or(u8::MAX);
            roots.push(RootEntry {
                idx: root_idx,
                path: spec.path.clone(),
                added_at_unix: spec.added_at_unix,
                doc_count: docs_per_root.get(&root_idx).copied().unwrap_or(0),
                primary: false,
            });
        }
    }

    let last_sweep_unix = match &state.status {
        WatchedFolderStatus::Idle {
            last_sweep_unix, ..
        } => *last_sweep_unix,
        WatchedFolderStatus::PausedManual { since_unix, .. } => *since_unix,
        WatchedFolderStatus::PausedAwaitingConfirmation {
            sweep_started_unix, ..
        } => *sweep_started_unix,
        WatchedFolderStatus::Errored { errored_unix, .. } => *errored_unix,
        WatchedFolderStatus::Sweeping { .. } => state.last_updated_unix,
    };

    // Folder-ingest v1 §3.3: project the persisted runtime
    // mirror onto the wire enum, threading the user-chosen
    // `pipeline_id` from the config side. The runtime status is
    // the source of truth for "what's actually happening";
    // pipeline_id comes from config because it survives even
    // when runtime is `Off` (e.g. between disable and re-
    // enable).
    //
    // Live progress (Building variants) flows through the
    // manager's in-memory map — the on-disk state file is only
    // refreshed on terminal transitions to avoid fsync churn at
    // every chapter event.
    let pipeline_id_for_enrichment = match cfg.source_type.watched_config().map(|w| &w.enrichment) {
        Some(sovereign_tools::local_corpus::config::WatchedEnrichmentConfig::On {
            pipeline_id,
            ..
        }) => Some(pipeline_id.clone()),
        _ => None,
    };
    let live_status = manager.enrichment_progress(&corpus_id);
    let runtime_status = live_status.as_ref().unwrap_or(&state.enrichment_status);
    let enrichment = match runtime_status {
        sovereign_tools::local_corpus::watched::state::EnrichmentRuntimeStatus::Off => {
            EnrichmentStatus::Off
        }
        sovereign_tools::local_corpus::watched::state::EnrichmentRuntimeStatus::Building {
            phase,
            current,
            total,
            started_at_unix,
        } => EnrichmentStatus::Building {
            pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
            phase: phase.clone(),
            current: *current,
            total: *total,
            started_at_unix: *started_at_unix,
        },
        sovereign_tools::local_corpus::watched::state::EnrichmentRuntimeStatus::Complete {
            built_at_unix,
            doc_count,
        } => EnrichmentStatus::Complete {
            pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
            built_at_unix: *built_at_unix,
            doc_count: *doc_count,
            current_doc_count: state.entries.len(),
        },
        sovereign_tools::local_corpus::watched::state::EnrichmentRuntimeStatus::Failed {
            failed_at_unix,
            reason,
        } => EnrichmentStatus::Failed {
            pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
            failed_at_unix: *failed_at_unix,
            reason: reason.clone(),
        },
        // Tiered (in-process) status — project into the existing
        // Off / Building / Complete API shapes so the desktop UI
        // doesn't need new variants today. Ready → Complete (with
        // `built_at_unix` from the Tiered payload). Failed inside
        // Tiered would have surfaced as the Failed arm above; this
        // arm is reached only for non-terminal states.
        sovereign_tools::local_corpus::watched::state::EnrichmentRuntimeStatus::Tiered {
            state: tier_state,
            started_at_unix,
            built_at_unix,
            doc_count,
        } => {
            use sovereign_core::types::AssetState;
            match tier_state {
                AssetState::Ready => EnrichmentStatus::Complete {
                    pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
                    built_at_unix: built_at_unix.unwrap_or(0),
                    doc_count: *doc_count,
                    current_doc_count: state.entries.len(),
                },
                AssetState::Failed { reason } => EnrichmentStatus::Failed {
                    pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
                    failed_at_unix: built_at_unix.unwrap_or(*started_at_unix),
                    reason: reason.clone(),
                },
                AssetState::Pending => EnrichmentStatus::Building {
                    pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
                    phase: "tiered:pending".into(),
                    current: 0,
                    total: 0,
                    started_at_unix: *started_at_unix,
                },
                AssetState::Indexing {
                    chunks_done,
                    chunks_total,
                } => EnrichmentStatus::Building {
                    pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
                    phase: "tiered:indexing".into(),
                    current: *chunks_done,
                    total: *chunks_total,
                    started_at_unix: *started_at_unix,
                },
                AssetState::PartiallyReady => EnrichmentStatus::Building {
                    pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
                    phase: "tiered:partially_ready".into(),
                    current: 0,
                    total: 0,
                    started_at_unix: *started_at_unix,
                },
                AssetState::BuildingSkeleton {
                    chunks_done,
                    chunks_total,
                } => EnrichmentStatus::Building {
                    pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
                    phase: "tiered:building_skeleton".into(),
                    current: *chunks_done,
                    total: *chunks_total,
                    started_at_unix: *started_at_unix,
                },
                AssetState::MultiHopReady => EnrichmentStatus::Building {
                    pipeline_id: pipeline_id_for_enrichment.clone().unwrap_or_default(),
                    phase: "tiered:multi_hop_ready".into(),
                    current: 0,
                    total: 0,
                    started_at_unix: *started_at_unix,
                },
            }
        }
    };

    Json(DetailsResponse {
        corpus_id: cfg.id,
        display_name: cfg.display_name,
        root_path: cfg.root_path,
        status: state.status,
        sync_mode,
        sensitive,
        live_entries: state.entries.len(),
        formats,
        skipped_by_extension: state.skipped_by_extension,
        failed_files: state.failed_files,
        tombstones: state.tombstones.len(),
        enrichment,
        last_sweep_unix,
        roots,
    })
    .into_response()
}

/// `GET /internal/corpus/watch/document/{corpus_id}/{doc_id}` —
/// the §3.7 per-document inspection digest. The `doc_id` segment
/// is URL-encoded by the caller (the relative path can contain
/// slashes that would otherwise break the route match).
async fn document_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((corpus_id, doc_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };

    // Tauri / Axum percent-decodes path segments by default; the doc_id
    // arrives as the raw relative path string the manager stored.
    let state = match manager.watched_state(&corpus_id).await {
        Ok(s) => s,
        Err(e) => return error(StatusCode::NOT_FOUND, format!("{e}")).into_response(),
    };
    let entry = match state.entries.get(&doc_id) {
        Some(e) => e.clone(),
        None => {
            return error(
                StatusCode::NOT_FOUND,
                format!("doc_id '{doc_id}' not in corpus '{corpus_id}' state"),
            )
            .into_response();
        }
    };

    // Chunk summary is best-effort — a doc_id that's listed in
    // `state.entries` but whose chunks haven't been written yet
    // (initial sweep mid-flight) returns `Ok((0, None))`. We
    // surface that as `chunk_count: 0` rather than 500-erroring
    // so the panel renders the rest of the metadata.
    let (chunk_count, first_chunk_preview) = manager
        .watched_doc_summary(&corpus_id, &doc_id, 500)
        .await
        .unwrap_or((0, None));

    Json(DocumentResponse {
        corpus_id,
        doc_id,
        absolute_path: entry.absolute_path,
        size_bytes: entry.size_bytes,
        mtime_unix: entry.mtime_unix,
        content_hash: entry.content_hash,
        chunk_count,
        first_chunk_preview,
        atoms: Vec::new(),
    })
    .into_response()
}

async fn incomplete_jobs_handler(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    let jobs = manager.watched_incomplete_jobs().await;
    Json(IncompleteJobsResponse { jobs }).into_response()
}

async fn pause_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    body: Option<Json<PauseRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    let reason = body
        .map(|Json(b)| b.reason)
        .unwrap_or_default()
        .unwrap_or_else(|| "user".into());
    match manager.pause_watched(&corpus_id, reason).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

async fn resume_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.resume_watched(&corpus_id).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

async fn confirm_deletion_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.confirm_pending_deletion(&corpus_id).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

/// `POST /internal/corpus/watch/{corpus_id}/roots` — folder-ingest
/// v1 §3.1, layer an additional root onto an existing watched
/// corpus. Body: `{ "path": "/abs/path/to/folder" }`. The next
/// scheduler tick walks the new root automatically.
async fn add_root_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    Json(req): Json<AddRootRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.add_watched_root(&corpus_id, req.path).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

/// `DELETE /internal/corpus/watch/{corpus_id}/roots/{idx}` —
/// detach an additional root by 0-based index. The next sweep
/// classifies the removed root's entries as deletions; the
/// existing deletion-guard semantics apply (a large root removal
/// trips the threshold and pauses for `confirm-deletion`).
async fn remove_root_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((corpus_id, idx)): Path<(String, usize)>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.remove_watched_root(&corpus_id, idx).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

/// `POST /internal/corpus/watch/{corpus_id}/enrich/enable` —
/// folder-ingest v1 §3.3, kick off an atlas build for a watched
/// folder using the requested pipeline. The build runs in a
/// subprocess; this handler returns immediately with a `job_id`
/// the UI can correlate with progress events.
async fn enrich_enable_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    Json(req): Json<EnrichEnableRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager
        .enable_enrichment(&corpus_id, &req.pipeline_id)
        .await
    {
        Ok(job_id) => Json(EnrichJobAck {
            corpus_id,
            job_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

/// `POST /internal/corpus/watch/{corpus_id}/enrich/disable` —
/// folder-ingest v1 §3.3, cancel any in-flight build, tear down
/// the atlas directory, and reset config + state to `Off`.
/// Idempotent.
async fn enrich_disable_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.disable_enrichment(&corpus_id).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

/// `POST /internal/corpus/watch/{corpus_id}/enrich/rebuild` —
/// folder-ingest v1 §3.3, re-run the same pipeline that's
/// currently configured. Errors when the corpus has no
/// enrichment configured (the user must `enable` first to pick a
/// pipeline).
async fn enrich_rebuild_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.rebuild_enrichment(&corpus_id).await {
        Ok(job_id) => Json(EnrichJobAck {
            corpus_id,
            job_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

/// `POST /internal/corpus/enrich-once` — body is a `LocalCorpusConfig`.
/// Registers a one-shot corpus (a drag-drop DocumentFolder the desktop
/// ingested with its own provider-less manager) into the daemon's
/// tiered-capable manager, then kicks off a single tiered enrichment
/// build. Deliberately does NOT add it to the sweep registry — no
/// watcher, just the initial atlas. Idempotent on the register; uses the
/// id `register` returns (path-identity may canonicalise it).
async fn enrich_once_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(cfg): Json<LocalCorpusConfig>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    use corpus_engine::enrichment::state::{
        EnrichmentHeartbeat, EnrichmentPhase, EnrichmentStateFile,
    };

    // Register the folder in the daemon's manager (idempotent). We deliberately
    // do NOT add it to the sweep registry — this is watched-folder registration
    // WITHOUT the watcher.
    let corpus_id = match manager.register(cfg).await {
        Ok(id) => id,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("register: {e}")).into_response(),
    };

    // The enrichment lifecycle is mirrored to `<index>/_enrichment_state.json`,
    // the SSOT that `GET /internal/enrichment/status` reads. Two invariants ride
    // on stamping it here — synchronously, BEFORE the blocking re-ingest:
    //
    //   1. Idempotency. A non-terminal phase means a prior enrich-once is still
    //      running (register → ingest → tiered build). Re-running would re-ingest
    //      and double-build. Short-circuit so a second trigger — the user
    //      clicking "Make explorable" again, or an auto-handoff racing a manual
    //      kick — is a no-op, not a duplicate job.
    //   2. Glassbox continuity. Without an early stamp the state file is absent
    //      for the whole register+ingest window, so the desktop can't tell a
    //      build is in flight: it re-offers "Make explorable" and a click
    //      double-kicks. Stamping `Starting` up front keeps status non-null from
    //      t=0; the tiered pipeline's own sink takes over the phase transitions
    //      once it begins scanning.
    let index_dir = manager.index_dir_root().join(&corpus_id);
    if let Ok(Some(state)) = EnrichmentStateFile::read(&index_dir) {
        // "In flight" only counts if the state is FRESH. A non-terminal
        // phase that hasn't advanced within the stall threshold is wedged —
        // a prior build that crashed, was killed, or (historically) fell
        // back to a doomed subprocess without ever stamping a terminal
        // phase. Treating a wedged `Starting` as live would block every
        // retry, so a stale state falls through to a fresh build below.
        // Same `is_stale` the status endpoint uses to surface the stall.
        let now = now_unix_secs();
        if !state.phase.is_terminal() && !state.is_stale(now) {
            tracing::info!(
                corpus_id = %corpus_id, phase = ?state.phase,
                "enrich-once: a build is already in flight — no-op (idempotent)"
            );
            return Json(EnrichOnceAck {
                corpus_id,
                files_indexed: 0,
                chunks_written: 0,
                ok: true,
                already_running: true,
            })
            .into_response();
        }
        if !state.phase.is_terminal() {
            let age_secs = now.saturating_sub(state.last_progress_at);
            tracing::warn!(
                corpus_id = %corpus_id, phase = ?state.phase, age_secs,
                "enrich-once: stale non-terminal enrichment state ({age_secs}s since last progress) — superseding with a fresh build"
            );
        }
    }
    let _ = std::fs::create_dir_all(&index_dir);
    if let Err(e) = EnrichmentStateFile::stamp(
        &index_dir,
        &corpus_id,
        Some("folder_tiered"),
        EnrichmentPhase::Starting,
        0,
        0,
        Some("Preparing to build the map"),
    ) {
        tracing::warn!(corpus_id = %corpus_id, "enrich-once: could not stamp Starting state: {e}");
    }

    // The re-ingest below re-embeds the corpus (acquire → chunk → embed →
    // index) and can run for many minutes on a large vault — all while the phase
    // would otherwise still read the initial `Starting` stamp above. Two things
    // keep the glassbox honest and the stall detector from false-firing across
    // that window:
    //   1. A `Scanning` phase stamp + a throttled per-batch progress callback,
    //      so the UI reads "Scanning documents" with a moving bar instead of a
    //      frozen "Preparing to build the map".
    //   2. An `EnrichmentHeartbeat` that bumps `last_progress_at` on a timer, so
    //      a phase quiet on its own progress events can never cross
    //      `STALL_THRESHOLD_SECS` and read as wedged. The guard drops when this
    //      handler returns; the spawned tiered build installs its own heartbeat
    //      for the GliNER + RAPTOR window, so liveness coverage is continuous.
    if let Err(e) = EnrichmentStateFile::stamp(
        &index_dir,
        &corpus_id,
        Some("folder_tiered"),
        EnrichmentPhase::Scanning,
        0,
        0,
        Some("Reading and embedding your notes"),
    ) {
        tracing::warn!(corpus_id = %corpus_id, "enrich-once: could not stamp Scanning state: {e}");
    }
    let _ingest_heartbeat = EnrichmentHeartbeat::spawn(index_dir.clone());
    let ingest_progress: sovereign_tools::local_corpus::manager::ProgressCallback = {
        use std::sync::atomic::{AtomicU64, Ordering};
        let stamp_dir = index_dir.clone();
        let stamp_corpus = corpus_id.clone();
        // The callback fires per chunk/batch, but each write is a tmp-file +
        // rename — throttle to ~50 writes across the whole embed pass.
        let last_bucket = std::sync::Arc::new(AtomicU64::new(u64::MAX));
        std::sync::Arc::new(
            move |evt: sovereign_tools::local_corpus::LocalCorpusProgress| {
                if let sovereign_tools::local_corpus::LocalCorpusProgress::Ingesting {
                    done,
                    total,
                    ..
                } = evt
                {
                    if total == 0 {
                        return;
                    }
                    let bucket = done.saturating_mul(50) / total;
                    if last_bucket.swap(bucket, Ordering::Relaxed) == bucket {
                        return;
                    }
                    let _ = EnrichmentStateFile::stamp(
                        &stamp_dir,
                        &stamp_corpus,
                        Some("folder_tiered"),
                        EnrichmentPhase::Scanning,
                        done,
                        total,
                        Some("Reading and embedding your notes"),
                    );
                }
            },
        )
    };

    // Ingest in the daemon (blocking) so the SAME process that writes the index
    // is the one that reads it to enrich. Doing the ingest in the desktop and
    // the enrich here deadlocked `enable_enrichment`'s index open on the
    // cross-process handoff — keeping writer and reader in one process is the fix.
    let stats = match manager
        .ingest(&corpus_id, None, Some(ingest_progress))
        .await
    {
        Ok(s) => s,
        Err(e) => {
            // Don't strand the state file at Starting — surface Failed so the
            // UI stops spinning and can re-offer the build.
            let _ = EnrichmentStateFile::fail(&index_dir, &corpus_id, &format!("ingest: {e}"));
            return error(StatusCode::INTERNAL_SERVER_ERROR, format!("ingest: {e}"))
                .into_response();
        }
    };
    // Enrich in the background — RAPTOR is slow and reads the index we just
    // wrote (in-process, no handoff). Fire-and-forget; the atlas status is
    // pollable via /internal/enrichment/status.
    let mgr = manager.clone();
    let id = corpus_id.clone();
    let fail_dir = index_dir.clone();
    let fail_id = corpus_id.clone();
    tokio::spawn(async move {
        match mgr.enrich_now(&id).await {
            Ok(job) => tracing::info!(corpus_id = %id, job, "enrich-once: tiered build started"),
            Err(e) => {
                tracing::warn!(corpus_id = %id, "enrich-once: enrichment did not start: {e}");
                // The tiered sink never took over — mark Failed so status
                // doesn't hang on the Starting stamp we wrote above.
                let _ =
                    EnrichmentStateFile::fail(&fail_dir, &fail_id, &format!("enrich start: {e}"));
            }
        }
    });
    Json(EnrichOnceAck {
        corpus_id,
        files_indexed: stats.files_indexed,
        chunks_written: stats.chunks_written,
        ok: true,
        already_running: false,
    })
    .into_response()
}

/// `POST /internal/corpus/enrich-reset` — body `{ "corpus_id": "…" }`.
/// Clears the "zombie" enrichment + watched-folder status a crashed,
/// killed, or stalled build leaves behind (the "Preparing to build the
/// map" that never advances, or a sticky `Errored` sweep) so the corpus
/// drops back to "no map yet" and can be rebuilt / swept again. Does NOT
/// touch the atlas or the index — only the status surfaces. Idempotent.
async fn enrich_reset_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<EnrichResetRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.reset_enrichment_state(&req.corpus_id).await {
        Ok(()) => Json(AckResponse {
            corpus_id: req.corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("reset: {e}")).into_response(),
    }
}

/// Body for [`reenrich_note_handler`]. `corpus_id` rides in the path;
/// only the note id is in the body. The correction hint itself is NOT
/// sent here — the desktop persists it to `conv_summary_corrections`
/// before calling, and the provider reads it back during the build.
#[derive(Debug, serde::Deserialize)]
struct ReenrichNoteRequest {
    source_doc_id: String,
}

/// `POST /internal/corpus/watch/{corpus_id}/enrich/reenrich-note` — body
/// `{ "source_doc_id": "…" }`. The "flag a wrong summary → re-enrich just
/// this note" revision loop (`docs/specs/SUMMARY_REVISION_LOOP.md`). Awaits
/// the (~1-min) single-note RAPTOR rebuild, which regenerates the summary
/// with the user's correction injected. Returns the driver's friendly
/// "busy" message (BAD_REQUEST) if a full build currently holds the permit.
async fn reenrich_note_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    Json(req): Json<ReenrichNoteRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.reenrich_note(&corpus_id, &req.source_doc_id).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

/// `POST /internal/corpus/watch/sync-now/{corpus_id}` — request a
/// Manual-mode sweep. Flips both the on-disk
/// `state.manual_sync_pending` mirror and the registry's in-memory
/// flag so the next scheduler tick dispatches the sweep.
///
/// Returns 409 Conflict for Continuous-mode corpora (where the
/// flag would silently no-op) and 400 BadRequest for unknown
/// corpora.
async fn sync_now_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    let Some(registry) = watched_folder_runtime::registry() else {
        return service_unavailable("watched-folder registry not installed").into_response();
    };
    // Persist the on-disk flag first so a daemon crash before the
    // registry write doesn't lose the pending request — auto-resume
    // on next start sees the flag and the registry can be repopulated
    // from there. Manager validates the Manual-mode requirement.
    if let Err(e) = manager.request_manual_sync(&corpus_id).await {
        let msg = format!("{e}");
        let code = if msg.contains("Continuous sync mode") {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        return error(code, msg).into_response();
    }
    let registered = registry.request_manual_sync(&corpus_id).await;
    if !registered {
        // Manager succeeded but registry doesn't have the corpus
        // yet — typical right after a process restart before
        // auto-resume completes. The on-disk flag still wins on the
        // next tick once the registry catches up.
        tracing::warn!(
            corpus_id = %corpus_id,
            "sync-now: registry slot missing; on-disk flag set, scheduler will pick up after auto-resume"
        );
    }
    Json(AckResponse {
        corpus_id,
        ok: true,
    })
    .into_response()
}

async fn remove_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    let Some(registry) = watched_folder_runtime::registry() else {
        return service_unavailable("watched-folder registry not installed").into_response();
    };
    registry.deregister(&corpus_id).await;
    match manager.remove(&corpus_id).await {
        Ok(()) => Json(AckResponse {
            corpus_id,
            ok: true,
        })
        .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────

fn service_unavailable(msg: &str) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorBody { error: msg.into() }),
    )
}

fn error(status: StatusCode, msg: String) -> (StatusCode, Json<ErrorBody>) {
    (status, Json(ErrorBody { error: msg }))
}

/// Wall-clock seconds since the Unix epoch, as `i64` to match
/// `EnrichmentState::last_progress_at`. Saturates to 0 before 1970.
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn basename_or_unknown(p: &std::path::Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "watched-folder".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_request_defaults_to_async() {
        let req: RegisterRequest = serde_json::from_str(r#"{"path":"/tmp/x"}"#).unwrap();
        assert_eq!(req.path, PathBuf::from("/tmp/x"));
        assert!(!req.sync_initial);
        // Default config matches WatchedFolderConfig::default — pinned
        // here so an HTTP caller that omits `config` gets the same
        // 120-second sweep cadence as the CLI.
        assert_eq!(req.config.sweep_interval_secs, 120);
    }

    #[test]
    fn register_request_full_body_parses() {
        // RegisterRequest is Deserialize-only (it crosses one
        // direction of the wire); pin that the full caller-shape
        // parses cleanly so a CLI omitting any field gets the
        // documented defaults.
        let body = serde_json::json!({
            "path": "/tmp/notes",
            "display_name": "My notes",
            "sync_initial": true,
        });
        let req: RegisterRequest = serde_json::from_value(body).expect("register body must parse");
        assert_eq!(req.path, PathBuf::from("/tmp/notes"));
        assert_eq!(req.display_name.as_deref(), Some("My notes"));
        assert!(req.sync_initial);
    }

    #[test]
    fn pause_request_accepts_empty_body() {
        let req: PauseRequest = serde_json::from_str("{}").unwrap();
        assert!(req.reason.is_none());
    }

    /// The desktop imports these as its HTTP CLIENT types
    /// (`sovereign-desktop::watched_folder_commands`), so `Deserialize` is
    /// load-bearing, not decoration. This test fails if someone trims the
    /// derive back to `Serialize` alone — which is the state that let seven
    /// hand-copied mirrors drift in the desktop until 2026-08-21 (nc-21).
    #[test]
    fn response_types_round_trip_for_the_desktop_client() {
        let wire = serde_json::to_string(&ListResponse {
            corpora: vec![ListEntry {
                corpus_id: "c1".into(),
                display_name: "Notes".into(),
                root_path: std::path::PathBuf::from("/tmp/notes"),
                status: WatchedFolderStatus::Idle {
                    last_sweep_unix: 7,
                    live_docs: 3,
                    tombstones: 1,
                },
                sync_mode: sovereign_tools::local_corpus::config::SyncMode::Manual,
                sensitive: true,
                additional_roots_count: 2,
            }],
        })
        .unwrap();

        let back: ListResponse = serde_json::from_str(&wire).unwrap();
        let e = &back.corpora[0];
        assert_eq!(e.corpus_id, "c1");
        // The three fields the desktop's hand-copy was missing.
        assert!(e.sensitive);
        assert_eq!(e.additional_roots_count, 2);
        assert_eq!(
            e.sync_mode,
            sovereign_tools::local_corpus::config::SyncMode::Manual
        );
        assert_eq!(
            serde_json::from_str::<AckResponse>(r#"{"corpus_id":"c1","ok":true}"#)
                .unwrap()
                .ok,
            true
        );
    }

    /// Attach mode can point the desktop at an older `sovereign daemon` that
    /// predates folder-ingest v1 and omits the three fields below. Before
    /// nc-21 the desktop's hand-copy didn't have them at all, so skew was
    /// invisible; now they are real fields and must degrade, not fail.
    #[test]
    fn list_entry_parses_a_pre_folder_ingest_v1_payload() {
        let old = r#"{"corpus_id":"c1","display_name":"Notes","root_path":"/tmp/notes",
                      "status":{"kind":"idle","last_sweep_unix":7,"live_docs":3,"tombstones":0}}"#;
        let e: ListEntry =
            serde_json::from_str(old).expect("older daemon payload must still parse");
        assert!(!e.sensitive);
        assert_eq!(e.additional_roots_count, 0);
        assert_eq!(
            e.sync_mode,
            sovereign_tools::local_corpus::config::SyncMode::Continuous
        );
    }
}
