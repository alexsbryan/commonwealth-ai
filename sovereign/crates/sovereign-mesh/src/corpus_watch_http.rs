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
            "/internal/corpus/watch/{corpus_id}",
            delete(remove_handler),
        )
        .layer(axum::middleware::from_fn(crate::loopback_guard::loopback_only))
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

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub corpus_id: String,
    pub display_name: String,
    pub initial_sweep: InitialSweepStatus,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InitialSweepStatus {
    Skipped,
    Spawned { corpus_id: String },
    Completed { files_indexed: usize, chunks_written: u64 },
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub corpora: Vec<ListEntry>,
}

#[derive(Debug, Serialize)]
pub struct ListEntry {
    pub corpus_id: String,
    pub display_name: String,
    pub root_path: PathBuf,
    pub status: WatchedFolderStatus,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub corpus_id: String,
    pub status: WatchedFolderStatus,
}

/// Richer status response — includes the per-extension skipped
/// breakdown and the failed-file detail. Returned from the
/// `/state/{corpus_id}` route. Kept separate from `StatusResponse`
/// so a polling caller doesn't pay for the larger payload on every
/// tick.
#[derive(Debug, Serialize)]
pub struct StateResponse {
    pub corpus_id: String,
    pub status: WatchedFolderStatus,
    pub skipped_by_extension: std::collections::HashMap<String, usize>,
    pub failed_files: Vec<FailedFile>,
    pub tombstones: usize,
    pub live_entries: usize,
}

#[derive(Debug, Serialize)]
pub struct IncompleteJobsResponse {
    pub jobs: Vec<WatchedIncompleteJob>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PauseRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AckResponse {
    pub corpus_id: String,
    pub ok: bool,
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
        return r.into_response();
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
    let cfg = LocalCorpusConfig::watched_folder(req.path.clone(), display_name.clone(), req.config.clone());
    let corpus_id = cfg.id.clone();
    let sweep_interval = req.config.sweep_interval_secs;

    if let Err(e) = manager.register(cfg).await {
        return error(StatusCode::INTERNAL_SERVER_ERROR, format!("register: {e}")).into_response();
    }

    // Register in the scheduler's registry so the next tick picks
    // it up. Idempotent — re-registering refreshes the cadence.
    registry.register(corpus_id.clone(), sweep_interval).await;

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
        return r.into_response();
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };

    let corpora = manager.list_watched().await;
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
        entries.push(ListEntry {
            corpus_id: cfg.id,
            display_name: cfg.display_name,
            root_path: cfg.root_path,
            status,
        });
    }
    Json(ListResponse { corpora: entries }).into_response()
}

async fn status_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
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
        return r.into_response();
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

async fn incomplete_jobs_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
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
        return r.into_response();
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    let reason = body
        .map(|Json(b)| b.reason)
        .unwrap_or_default()
        .unwrap_or_else(|| "user".into());
    match manager.pause_watched(&corpus_id, reason).await {
        Ok(()) => Json(AckResponse { corpus_id, ok: true }).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

async fn resume_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.resume_watched(&corpus_id).await {
        Ok(()) => Json(AckResponse { corpus_id, ok: true }).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

async fn confirm_deletion_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    match manager.confirm_pending_deletion(&corpus_id).await {
        Ok(()) => Json(AckResponse { corpus_id, ok: true }).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

async fn remove_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    let Some(manager) = watched_folder_runtime::manager() else {
        return service_unavailable("watched-folder runtime not installed").into_response();
    };
    let Some(registry) = watched_folder_runtime::registry() else {
        return service_unavailable("watched-folder registry not installed").into_response();
    };
    registry.deregister(&corpus_id).await;
    match manager.remove(&corpus_id).await {
        Ok(()) => Json(AckResponse { corpus_id, ok: true }).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────

fn enforce_localhost(addr: &SocketAddr) -> Result<(), (StatusCode, Json<ErrorBody>)> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(ErrorBody {
                error: "local-only".into(),
            }),
        ))
    }
}

fn service_unavailable(msg: &str) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorBody { error: msg.into() }),
    )
}

fn error(status: StatusCode, msg: String) -> (StatusCode, Json<ErrorBody>) {
    (status, Json(ErrorBody { error: msg }))
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
        let req: RegisterRequest =
            serde_json::from_value(body).expect("register body must parse");
        assert_eq!(req.path, PathBuf::from("/tmp/notes"));
        assert_eq!(req.display_name.as_deref(), Some("My notes"));
        assert!(req.sync_initial);
    }

    #[test]
    fn pause_request_accepts_empty_body() {
        let req: PauseRequest = serde_json::from_str("{}").unwrap();
        assert!(req.reason.is_none());
    }
}
