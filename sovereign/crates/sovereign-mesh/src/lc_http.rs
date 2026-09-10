// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-corpus HTTP — `/internal/corpus/local/...` (sv-surface D5).
//!
//! The non-watch half of `LocalCorpusManager`, served over the
//! daemon's OWN manager — the same `watched_folder_runtime::manager()`
//! singleton `corpus_watch_http` has served seventeen watch routes
//! over since folder-ingest v1. Nothing new is constructed here and no
//! second manager is installed: this file is doors, not machinery.
//!
//! # Why this exists
//!
//! `local_corpus_commands.rs` builds a SECOND `LocalCorpusManager` in
//! the desktop process (`AppState.local_corpus`) and calls it
//! directly. In attach mode that means two managers over one vault —
//! two snapshot roots, two cluster caches, two writers to the same
//! index directory. The §10.6 twin the sv-surface campaign exists to
//! delete; these routes are what the desktop repoints onto so the
//! second manager can go.
//!
//! # Served
//!
//! | Desktop command | Route |
//! |---|---|
//! | `lc_ocr_available` | `GET    /internal/corpus/local/ocr-available` |
//! | `lc_list` | `GET    /internal/corpus/local` |
//! | (`manager.get`, the read inside `lc_enrich_now`) | `GET    /internal/corpus/local/{corpus}` |
//! | `lc_remove` | `DELETE /internal/corpus/local/{corpus}` |
//! | `lc_incomplete_jobs` | `GET    /internal/corpus/local/incomplete-jobs` |
//! | `lc_cancel` | `POST   /internal/corpus/local/{corpus}/cancel` |
//! | `lc_check_git` | `GET    /internal/corpus/local/{corpus}/git` |
//! | `lc_write_tags` | `POST   /internal/corpus/local/{corpus}/write-tags` |
//! | `lc_list_snapshots` | `GET    /internal/corpus/local/{corpus}/snapshots` |
//! | `lc_rollback` | `POST   /internal/corpus/local/{corpus}/rollback` |
//! | `lc_clean` | `POST   /internal/corpus/local/{corpus}/clean` |
//! | `lc_get_preview` | `POST   /internal/corpus/local/{corpus}/preview` |
//! | `lc_search` | `POST   /internal/corpus/local/{corpus}/search` |
//! | `lc_ingest` | `POST   /internal/corpus/local/{corpus}/ingest` |
//!
//! Grouped by input shape, not by verb: everything that takes only a
//! corpus id is a GET or a bodyless POST on `{corpus}`; everything
//! that takes options carries them in a body. Two response types are
//! new (`OcrAvailability`, `CancelAck`, `IngestJobAck`) and one is a
//! wire twin by necessity (`LocalSearchHit` — see below); every other
//! answer is `sovereign_tools::local_corpus`'s own type, already
//! `Serialize + Deserialize`, so the desktop rung is a repoint.
//!
//! # `lc_ingest` is a job, not a request
//!
//! `manager.ingest` re-embeds a whole vault and runs for minutes. The
//! command already spawns and returns a job id immediately; the route
//! does the same (`64dfedb33`'s rung, and `corpus_watch_http`'s own
//! `EnrichJobAck` shape), because a synchronous route here would hold
//! a connection open past every client timeout in the estate.
//!
//! **No new job table.** Progress is stamped into the corpus's
//! `EnrichmentStateFile` by `corpus_watch_http::ingest_progress_stamper`
//! — ONE implementation, shared with `enrich-once` — and reported by
//! the EXISTING `GET /internal/corpus/watch/status/{corpus}` route
//! (and by commonwealth's `/internal/enrichment/status`). A second job
//! table beside that would be a second answer to "how far along is
//! this corpus", which is exactly the smell this campaign is deleting.
//!
//! # NOT served, named rather than dropped (ARCH §18.3)
//!
//! - `lc_validate_path` and `lc_pre_scan` are app-local by decision:
//!   both probe a path the USER just picked in a file dialog, before
//!   any corpus exists. `lc_pre_scan` additionally `register`s that
//!   path, which is why no bare `register` route is offered either —
//!   there is no caller for one that is not the pre-scan flow.
//! - `lc_cluster` is a job whose ONLY output channel is the desktop's
//!   Tauri progress emitter. The manager writes no state file for
//!   clustering, so serving it would require minting the job table the
//!   section above refuses. It stays app-local until clustering has a
//!   daemon-side reporter.
//! - `lc_enrich_now`, `lc_enrich_reset` and `lc_reenrich_note` are
//!   already HTTP proxies to this daemon — `corpus_watch_http` serves
//!   all three. Their one remaining local read (`manager.get`) is the
//!   `GET /internal/corpus/local/{corpus}` route above.
//!
//! Loopback posture is `corpus_watch_http`'s, unchanged: router-level
//! [`crate::loopback_guard::loopback_only`] middleware plus a
//! per-handler `enforce_localhost`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Json, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use sovereign_tools::local_corpus::clusterer::ClusterConfig;
use sovereign_tools::local_corpus::config::LocalCorpusConfig;
use sovereign_tools::local_corpus::LocalCorpusManager;

use crate::loopback_guard::enforce_localhost;
use crate::watched_folder_runtime;

// ─── Wire shapes ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

/// Answer of `GET /internal/corpus/local/ocr-available`. A named
/// field, not a bare `true`: "OCR is unavailable" and "this daemon did
/// not understand the question" must not both read as `false`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OcrAvailability {
    pub available: bool,
}

/// Answer of `POST …/{corpus}/cancel`. `cancelled` is "there WAS an
/// in-flight job and it is now cancelled" — deliberately not the
/// `AckResponse.ok` field, which means "the call succeeded". Both are
/// true for a cancel that found nothing to cancel, and collapsing them
/// would tell the pane a job was stopped when none was running.
#[derive(Debug, Serialize, Deserialize)]
pub struct CancelAck {
    pub corpus_id: String,
    pub cancelled: bool,
}

/// Answer of `POST …/{corpus}/ingest` — the job id, and where to read
/// its progress. `corpus_watch_http::EnrichJobAck`'s shape plus the
/// route that reports it, because a job id with no named reporter is
/// how a caller ends up inventing a poll loop of its own.
#[derive(Debug, Serialize, Deserialize)]
pub struct IngestJobAck {
    pub corpus_id: String,
    pub job_id: String,
    pub ok: bool,
    /// The route that reports this job. Always populated.
    pub progress_route: String,
}

/// Body of `POST …/{corpus}/ingest`.
#[derive(Debug, Default, Deserialize)]
pub struct IngestRequest {
    /// `None` leaves the choice to the corpus's own config — the
    /// command's `Option` semantics, unchanged.
    #[serde(default)]
    pub with_ocr: Option<bool>,
}

/// Body of `POST …/{corpus}/write-tags`.
#[derive(Debug, Default, Deserialize)]
pub struct WriteTagsRequest {
    /// The command's `git_commit.unwrap_or(false)` — the default lives
    /// here now, one decider.
    #[serde(default)]
    pub git_commit: Option<bool>,
}

/// Body of `POST …/{corpus}/rollback`.
#[derive(Debug, Deserialize)]
pub struct RollbackRequest {
    /// A path the caller got from `…/snapshots`. It is NOT a
    /// user-picked path: every value the pane can send came out of
    /// this daemon's own snapshot listing.
    pub snapshot_path: String,
}

/// Body of `POST …/{corpus}/preview`.
#[derive(Debug, Default, Deserialize)]
pub struct PreviewRequest {
    /// `None` is `ClusterConfig::default()` — the command's
    /// `config.unwrap_or_default()`, kept.
    #[serde(default)]
    pub config: Option<ClusterConfig>,
}

/// Body of `POST …/{corpus}/search`.
#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    /// `None` is 10 — the command's `limit.unwrap_or(10)`.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One search hit. A wire twin of the desktop's `LocalSearchHit` by
/// NECESSITY, not by choice: `manager.search` answers
/// `Vec<ScoredChunk>`, and `ScoredChunk` is deliberately
/// non-serialisable ("in-process ranking currency only",
/// `sovereign-contracts/src/types/mod.rs`). The desktop already
/// projects into exactly these four fields before handing them to the
/// pane; the projection moves here and the name is kept so the repoint
/// is a changed `use`, not a changed call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSearchHit {
    pub content: String,
    pub title: Option<String>,
    pub corpus_id: String,
    pub score: f32,
}

// ─── Router ────────────────────────────────────────────────────

/// The local-corpus router. Mounted beside `corpus_watch_http` on the
/// daemon's client router; a daemon with no local-corpus runtime
/// answers 503 with that named reason on every route, which is a
/// different fact from "the route is not mounted".
pub fn lc_router() -> Router {
    Router::new()
        .route("/internal/corpus/local", get(list))
        .route("/internal/corpus/local/ocr-available", get(ocr_available))
        .route(
            "/internal/corpus/local/incomplete-jobs",
            get(incomplete_jobs),
        )
        .route(
            "/internal/corpus/local/{corpus_id}",
            get(get_one).delete(remove),
        )
        .route("/internal/corpus/local/{corpus_id}/cancel", post(cancel))
        .route("/internal/corpus/local/{corpus_id}/git", get(check_git))
        .route(
            "/internal/corpus/local/{corpus_id}/write-tags",
            post(write_tags),
        )
        .route(
            "/internal/corpus/local/{corpus_id}/snapshots",
            get(list_snapshots),
        )
        .route(
            "/internal/corpus/local/{corpus_id}/rollback",
            post(rollback),
        )
        .route("/internal/corpus/local/{corpus_id}/clean", post(clean))
        .route("/internal/corpus/local/{corpus_id}/preview", post(preview))
        .route("/internal/corpus/local/{corpus_id}/search", post(search))
        .route("/internal/corpus/local/{corpus_id}/ingest", post(ingest))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
}

// ─── Handlers: the whole-registry reads ────────────────────────

/// GET /internal/corpus/local — every registered local corpus. Wire
/// form of `lc_list`; answers `Vec<LocalCorpusConfig>`.
///
/// An empty list is a real answer (a fresh install), which is why the
/// absent RUNTIME has to be a 503 rather than one.
async fn list(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let rows = manager.list().await;
    tracing::debug!(corpora = rows.len(), "lc_http: local corpora listed");
    (StatusCode::OK, Json(rows)).into_response()
}

/// GET /internal/corpus/local/ocr-available — whether this daemon can
/// OCR a scanned PDF. Wire form of `lc_ocr_available`.
///
/// The command degrades a missing manager to `false`; the route does
/// not. "No OCR context installed" and "no local-corpus runtime at
/// all" are different facts and the pane offers a different remedy for
/// each (§18.3).
async fn ocr_available(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    (
        StatusCode::OK,
        Json(OcrAvailability {
            available: manager.ocr_available().await,
        }),
    )
        .into_response()
}

/// GET /internal/corpus/local/incomplete-jobs — every ingest that
/// started and never finished. Wire form of `lc_incomplete_jobs`;
/// answers `Vec<IncompleteJob>`.
async fn incomplete_jobs(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    (StatusCode::OK, Json(manager.incomplete_jobs().await)).into_response()
}

// ─── Handlers: one corpus ──────────────────────────────────────

/// GET /internal/corpus/local/{corpus} — one registered corpus's
/// config. The read `lc_enrich_now` does locally before it POSTs the
/// config back to this same daemon.
///
/// A corpus the manager does not carry is a 404 that names it, never a
/// 200 with a defaulted config.
async fn get_one(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    match manager.get(&corpus_id).await {
        Some(cfg) => (StatusCode::OK, Json(cfg)).into_response(),
        None => not_registered(&corpus_id),
    }
}

/// DELETE /internal/corpus/local/{corpus} — unregister and drop the
/// index. Wire form of `lc_remove`.
async fn remove(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    match manager.remove(&corpus_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(&format!("remove: {e}")),
    }
}

/// POST /internal/corpus/local/{corpus}/cancel — ask an in-flight
/// ingest to stop. Wire form of `lc_cancel`.
async fn cancel(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let cancelled = manager.cancel(&corpus_id);
    (
        StatusCode::OK,
        Json(CancelAck {
            corpus_id,
            cancelled,
        }),
    )
        .into_response()
}

/// GET /internal/corpus/local/{corpus}/git — is the vault a git
/// worktree, and is it clean? Wire form of `lc_check_git`; answers
/// `Option<GitStatus>` (an explicit `null` = not a repository).
async fn check_git(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    match manager.check_git(&corpus_id).await {
        Ok(status) => (StatusCode::OK, Json(status)).into_response(),
        Err(e) => internal_error(&format!("check_git: {e}")),
    }
}

/// POST /internal/corpus/local/{corpus}/write-tags — write the
/// clustered tags back into the vault's front-matter. Wire form of
/// `lc_write_tags`; answers `WriteBackResult`.
async fn write_tags(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    body: Option<Json<WriteTagsRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let git_commit = body.and_then(|Json(b)| b.git_commit).unwrap_or(false);
    match manager.write_tags(&corpus_id, git_commit).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => internal_error(&format!("write_tags: {e}")),
    }
}

/// GET /internal/corpus/local/{corpus}/snapshots — the pre-write-back
/// snapshots this daemon holds. Wire form of `lc_list_snapshots`;
/// answers `Vec<SnapshotMeta>`.
async fn list_snapshots(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    match manager.list_snapshots(&corpus_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => internal_error(&format!("list_snapshots: {e}")),
    }
}

/// POST /internal/corpus/local/{corpus}/rollback — restore one
/// snapshot. Wire form of `lc_rollback`; answers `RollbackResult`.
async fn rollback(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    Json(req): Json<RollbackRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let path = std::path::PathBuf::from(&req.snapshot_path);
    match manager.rollback(&corpus_id, &path).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => internal_error(&format!("rollback: {e}")),
    }
}

/// POST /internal/corpus/local/{corpus}/clean — strip written-back
/// tags from the vault. Wire form of `lc_clean`; answers `CleanResult`.
async fn clean(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    match manager.clean(&corpus_id).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => internal_error(&format!("clean: {e}")),
    }
}

/// POST /internal/corpus/local/{corpus}/preview — what write-back
/// WOULD do, from the cached cluster result. Wire form of
/// `lc_get_preview`; answers `VaultPreview`.
///
/// A POST because the input is a `ClusterConfig`, which carries
/// thresholds no flat query string expresses — `atlas_http`'s
/// precedent: a read, asked with a body.
async fn preview(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    body: Option<Json<PreviewRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let cfg = body
        .and_then(|Json(b)| b.config)
        .unwrap_or_else(ClusterConfig::default);
    match manager.get_preview(&corpus_id, &cfg).await {
        Ok(preview) => (StatusCode::OK, Json(preview)).into_response(),
        Err(e) => internal_error(&format!("get_preview: {e}")),
    }
}

/// POST /internal/corpus/local/{corpus}/search — search one local
/// corpus. Wire form of `lc_search`; answers `Vec<LocalSearchHit>`.
async fn search(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    Json(req): Json<SearchRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let limit = req.limit.unwrap_or(10);
    match manager.search(&corpus_id, &req.query, limit).await {
        Ok(hits) => {
            let out: Vec<LocalSearchHit> = hits
                .into_iter()
                .map(|c| LocalSearchHit {
                    content: c.content,
                    title: c.title,
                    corpus_id: c.corpus_id,
                    score: c.score,
                })
                .collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => internal_error(&format!("search: {e}")),
    }
}

/// POST /internal/corpus/local/{corpus}/ingest — submit the ingest as
/// a job. Wire form of `lc_ingest`; answers `IngestJobAck`
/// immediately with `202 Accepted`.
///
/// The corpus must be registered BEFORE the spawn: a 404 for an
/// unknown id has to arrive on this response, not fifteen seconds
/// later in a log nobody reads.
async fn ingest(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(corpus_id): Path<String>,
    body: Option<Json<IngestRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_or_503() {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    if manager.get(&corpus_id).await.is_none() {
        return not_registered(&corpus_id);
    }
    let with_ocr = body.and_then(|Json(b)| b.with_ocr);

    let index_dir = manager.index_dir_root().join(&corpus_id);
    let _ = std::fs::create_dir_all(&index_dir);
    let job_id = format!("lc-ingest-{}", uuid::Uuid::new_v4());
    let progress =
        crate::corpus_watch_http::ingest_progress_stamper(index_dir.clone(), corpus_id.clone());

    let spawn_corpus = corpus_id.clone();
    let spawn_job = job_id.clone();
    tokio::spawn(async move {
        match manager
            .ingest(&spawn_corpus, with_ocr, Some(progress))
            .await
        {
            Ok(stats) => tracing::info!(
                corpus_id = %spawn_corpus,
                job_id = %spawn_job,
                files_indexed = stats.files_indexed,
                chunks_written = stats.chunks_written,
                "lc_http: ingest job finished"
            ),
            Err(e) => {
                tracing::warn!(
                    corpus_id = %spawn_corpus,
                    job_id = %spawn_job,
                    "lc_http: ingest job failed: {e}"
                );
                // Don't strand the phase file mid-Scanning — a pane
                // polling the status route would spin forever on a
                // job that is already dead.
                let _ = corpus_engine::enrichment::state::EnrichmentStateFile::fail(
                    &index_dir,
                    &spawn_corpus,
                    &format!("ingest: {e}"),
                );
            }
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(IngestJobAck {
            progress_route: format!("/internal/corpus/watch/status/{corpus_id}"),
            corpus_id,
            job_id,
            ok: true,
        }),
    )
        .into_response()
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's ONE local-corpus manager, or the named 503. Same
/// singleton `corpus_watch_http` reads — this file installs nothing.
fn manager_or_503() -> Result<Arc<LocalCorpusManager>, axum::response::Response> {
    match watched_folder_runtime::manager() {
        Some(m) => Ok(m),
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "local-corpus runtime not installed on this daemon".into(),
            }),
        )
            .into_response()),
    }
}

fn not_registered(corpus_id: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: format!("corpus '{corpus_id}' is not registered locally"),
        }),
    )
        .into_response()
}

fn internal_error(msg: &str) -> axum::response::Response {
    tracing::warn!("lc_http: {msg}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

/// Keeps `LocalCorpusConfig` named in this file's public surface: the
/// list and get routes answer it, and a reader looking for "what shape
/// comes back" should find the type without leaving the module.
#[allow(dead_code)]
fn _answers_local_corpus_config(c: LocalCorpusConfig) -> LocalCorpusConfig {
    c
}
