// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-corpus HTTP — `/internal/corpus/local/…` (sv-surface D5).
//!
//! The non-watch half of `LocalCorpusManager`, served over the daemon's OWN
//! `watched_folder_runtime::manager()` singleton — the one `corpus_watch_http`
//! has served its watch routes over since folder-ingest v1. Nothing is
//! constructed here: doors, not machinery. It exists because
//! `local_corpus_commands.rs` built a SECOND manager in the desktop process.
//! Routes group by input shape: a corpus id alone is a GET or bodyless POST
//! on `{corpus}`; options ride in a body. `lc_ingest` is a JOB — it re-embeds
//! a whole vault — so it answers a job id, and progress is read back from the
//! `EnrichmentStateFile` `corpus_watch_http::ingest_progress_stamper` already
//! stamps. No new job table.
//!
//! Loopback posture is `corpus_watch_http`'s, unchanged.
//!
//! NOT served, named rather than dropped (ARCH §18.3):
//! - `lc_validate_path` + the SCAN half of `lc_pre_scan` — both probe a path
//!   the USER just picked. (The REGISTRATION half does cross.)
//! - `lc_cluster` — its only output channel is the desktop's Tauri emitter.
//! - `lc_enrich_now`/`_reset`/`lc_reenrich_note` — `corpus_watch_http` serves
//!   all three already.

use std::sync::Arc;

use axum::extract::{Json, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use sovereign_tools::local_corpus::clusterer::ClusterConfig;
use sovereign_tools::local_corpus::config::LocalCorpusConfig;
use sovereign_tools::local_corpus::LocalCorpusManager;

use crate::http_response::{internal_error, not_found, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};
use crate::watched_folder_runtime;

// ─── Wire shapes ───────────────────────────────────────────────

/// The four answers this router's routes give that are pure serde over
/// primitives. Defined in `sovereign-contracts` so a client can parse them
/// without linking this crate, re-exported here so every route below, its
/// tests and the CLI keep naming them at this path (sv-surface svt-3).
pub use sovereign_contracts::daemon_wire::{
    CancelAck, IngestJobAck, LocalSearchHit, OcrAvailability,
};

/// What `GET …/{corpus}/ingest/progress` answers.
///
/// Joins the two files an ingest writes without collapsing them. `state`
/// is the phase file the shared stamper throttles into
/// (`_enrichment_state.json`) — how far along, live. `outcome` is the
/// terminal receipt (`_ingest_result.json`) — what the job INDEXED, or
/// why it did not, written once when the ingest half ends.
///
/// `finished` reads off `outcome`, NOT off the phase. On the
/// `enrich-once` path the phase file goes on to describe the atlas build
/// long after the ingest is done, and on the `lc_http` job path it stops
/// at `Scanning` forever because no enrichment follows. Either way
/// "has the ingest finished" is a question only the receipt answers.
///
/// Both fields are `Option` and neither substitutes for the other: no
/// phase file means no ingest has run in this index dir, and no outcome
/// means none has FINISHED — a running job has the first and not the
/// second (ARCH §18.3).
#[derive(Debug, Serialize, Deserialize)]
pub struct IngestProgress {
    pub corpus_id: String,
    /// The live phase stamp, when one exists.
    pub state: Option<corpus_engine::enrichment::state::EnrichmentState>,
    /// The terminal receipt, when the ingest half has ended.
    pub outcome: Option<crate::corpus_watch_http::IngestOutcome>,
    /// `true` iff `outcome` is present. Spelled out rather than left to
    /// the caller so two clients cannot disagree about what terminal
    /// means.
    pub finished: bool,
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

// ─── Router ────────────────────────────────────────────────────

/// The local-corpus router. Mounted beside `corpus_watch_http` on the
/// daemon's client router; a daemon with no local-corpus runtime
/// answers 503 with that named reason on every route, which is a
/// different fact from "the route is not mounted".
pub fn lc_router() -> Router {
    Router::new()
        .route("/internal/corpus/local", get(list).post(register))
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
        .route(
            "/internal/corpus/local/{corpus_id}/ingest/progress",
            get(ingest_progress),
        )
        .localhost_only()
}

// ─── Handlers: the whole-registry reads ────────────────────────

/// GET /internal/corpus/local — every registered local corpus. Wire
/// form of `lc_list`; answers `Vec<LocalCorpusConfig>`.
///
/// An empty list is a real answer (a fresh install), which is why the
/// absent RUNTIME has to be a 503 rather than one.
async fn list(_: LocalOnly) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    let rows = manager.list().await;
    tracing::debug!(corpora = rows.len(), "lc_http: local corpora listed");
    Ok((StatusCode::OK, Json(rows)).into_response())
}

/// GET /internal/corpus/local/ocr-available — whether this daemon can
/// OCR a scanned PDF. Wire form of `lc_ocr_available`.
///
/// The command degrades a missing manager to `false`; the route does
/// not. "No OCR context installed" and "no local-corpus runtime at
/// all" are different facts and the pane offers a different remedy for
/// each (§18.3).
async fn ocr_available(_: LocalOnly) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    Ok((
        StatusCode::OK,
        Json(OcrAvailability {
            available: manager.ocr_available().await,
        }),
    )
        .into_response())
}

/// GET /internal/corpus/local/incomplete-jobs — every ingest that
/// started and never finished. Wire form of `lc_incomplete_jobs`;
/// answers `Vec<IncompleteJob>`.
async fn incomplete_jobs(_: LocalOnly) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    Ok((StatusCode::OK, Json(manager.incomplete_jobs().await)).into_response())
}

/// POST /internal/corpus/local — register (or re-register) one local
/// corpus with THIS daemon's manager. Wire form of the `.register`
/// call `lc_pre_scan` used to make on the desktop's own manager.
///
/// # Idempotent, because `register` is
///
/// `LocalCorpusManager::register` overwrites on a repeat id and keeps
/// the EXISTING id when the path is already registered under one (its
/// path-identity guard). The route mirrors that rather than minting a
/// 409: a second pre-scan of the same folder is an ordinary thing for a
/// user to do, and answering 409 would make the pane treat it as an
/// error it cannot resolve.
///
/// # Why the answer is the config AS REGISTERED
///
/// Because the id the caller sent is not necessarily the id the manager
/// kept. The stored config carries the surviving id in its own `id`
/// field, so there is ONE name for it on the wire and no second field
/// to disagree with (§10.6). A caller that ingests under the id it sent,
/// rather than the id it got back, is the 404 this route exists to end.
async fn register(
    _: LocalOnly,
    Json(config): Json<LocalCorpusConfig>,
) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    let requested = config.id.clone();
    let root = config.root_path.clone();
    let id = match manager.register(config).await {
        Ok(id) => id,
        Err(e) => return Ok(log_and_500(&format!("register: {e}"))),
    };
    Ok(match manager.get(&id).await {
        Some(stored) => {
            tracing::info!(
                corpus_id = %id,
                requested = %requested,
                reused_existing_id = id != requested,
                root = %root.display(),
                "lc_http: local corpus registered"
            );
            (StatusCode::OK, Json(stored)).into_response()
        }
        // Not reachable through `register`, which inserts before it
        // returns. Reported rather than papered over with the config
        // that was SENT, which would echo an id the registry does not
        // hold (§18.3).
        None => log_and_500(&format!(
            "register: '{id}' is not in the registry immediately after being written"
        )),
    })
}

// ─── Handlers: one corpus ──────────────────────────────────────

/// GET /internal/corpus/local/{corpus} — one registered corpus's
/// config. The read `lc_enrich_now` does locally before it POSTs the
/// config back to this same daemon.
///
/// A corpus the manager does not carry is a 404 that names it, never a
/// 200 with a defaulted config.
async fn get_one(_: LocalOnly, Path(corpus_id): Path<String>) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    Ok(match manager.get(&corpus_id).await {
        Some(cfg) => (StatusCode::OK, Json(cfg)).into_response(),
        None => not_registered(&corpus_id),
    })
}

/// DELETE /internal/corpus/local/{corpus} — unregister and drop the
/// index. Wire form of `lc_remove`.
async fn remove(_: LocalOnly, Path(corpus_id): Path<String>) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    Ok(match manager.remove(&corpus_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => log_and_500(&format!("remove: {e}")),
    })
}

/// POST /internal/corpus/local/{corpus}/cancel — ask an in-flight
/// ingest to stop. Wire form of `lc_cancel`.
async fn cancel(_: LocalOnly, Path(corpus_id): Path<String>) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    let cancelled = manager.cancel(&corpus_id);
    Ok((
        StatusCode::OK,
        Json(CancelAck {
            corpus_id,
            cancelled,
        }),
    )
        .into_response())
}

/// GET /internal/corpus/local/{corpus}/git — is the vault a git
/// worktree, and is it clean? Wire form of `lc_check_git`; answers
/// `Option<GitStatus>` (an explicit `null` = not a repository).
async fn check_git(_: LocalOnly, Path(corpus_id): Path<String>) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    Ok(match manager.check_git(&corpus_id).await {
        Ok(status) => (StatusCode::OK, Json(status)).into_response(),
        Err(e) => log_and_500(&format!("check_git: {e}")),
    })
}

/// POST /internal/corpus/local/{corpus}/write-tags — write the
/// clustered tags back into the vault's front-matter. Wire form of
/// `lc_write_tags`; answers `WriteBackResult`.
async fn write_tags(
    _: LocalOnly,
    Path(corpus_id): Path<String>,
    body: Option<Json<WriteTagsRequest>>,
) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    let git_commit = body.and_then(|Json(b)| b.git_commit).unwrap_or(false);
    Ok(match manager.write_tags(&corpus_id, git_commit).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => log_and_500(&format!("write_tags: {e}")),
    })
}

/// GET /internal/corpus/local/{corpus}/snapshots — the pre-write-back
/// snapshots this daemon holds. Wire form of `lc_list_snapshots`;
/// answers `Vec<SnapshotMeta>`.
async fn list_snapshots(_: LocalOnly, Path(corpus_id): Path<String>) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    Ok(match manager.list_snapshots(&corpus_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => log_and_500(&format!("list_snapshots: {e}")),
    })
}

/// POST /internal/corpus/local/{corpus}/rollback — restore one
/// snapshot. Wire form of `lc_rollback`; answers `RollbackResult`.
async fn rollback(
    _: LocalOnly,
    Path(corpus_id): Path<String>,
    Json(req): Json<RollbackRequest>,
) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    let path = std::path::PathBuf::from(&req.snapshot_path);
    Ok(match manager.rollback(&corpus_id, &path).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => log_and_500(&format!("rollback: {e}")),
    })
}

/// POST /internal/corpus/local/{corpus}/clean — strip written-back
/// tags from the vault. Wire form of `lc_clean`; answers `CleanResult`.
async fn clean(_: LocalOnly, Path(corpus_id): Path<String>) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    Ok(match manager.clean(&corpus_id).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => log_and_500(&format!("clean: {e}")),
    })
}

/// POST /internal/corpus/local/{corpus}/preview — what write-back
/// WOULD do, from the cached cluster result. Wire form of
/// `lc_get_preview`; answers `VaultPreview`.
///
/// A POST because the input is a `ClusterConfig`, which carries
/// thresholds no flat query string expresses — `atlas_http`'s
/// precedent: a read, asked with a body.
async fn preview(
    _: LocalOnly,
    Path(corpus_id): Path<String>,
    body: Option<Json<PreviewRequest>>,
) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    let cfg = body
        .and_then(|Json(b)| b.config)
        .unwrap_or_else(ClusterConfig::default);
    Ok(match manager.get_preview(&corpus_id, &cfg).await {
        Ok(preview) => (StatusCode::OK, Json(preview)).into_response(),
        Err(e) => log_and_500(&format!("get_preview: {e}")),
    })
}

/// POST /internal/corpus/local/{corpus}/search — search one local
/// corpus. Wire form of `lc_search`; answers `Vec<LocalSearchHit>`.
async fn search(
    _: LocalOnly,
    Path(corpus_id): Path<String>,
    Json(req): Json<SearchRequest>,
) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    let limit = req.limit.unwrap_or(10);
    Ok(match manager.search(&corpus_id, &req.query, limit).await {
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
        Err(e) => log_and_500(&format!("search: {e}")),
    })
}

/// POST /internal/corpus/local/{corpus}/ingest — submit the ingest as
/// a job. Wire form of `lc_ingest`; answers `IngestJobAck`
/// immediately with `202 Accepted`.
///
/// The corpus must be registered BEFORE the spawn: a 404 for an
/// unknown id has to arrive on this response, not fifteen seconds
/// later in a log nobody reads.
async fn ingest(
    _: LocalOnly,
    Path(corpus_id): Path<String>,
    body: Option<Json<IngestRequest>>,
) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    if manager.get(&corpus_id).await.is_none() {
        return Ok(not_registered(&corpus_id));
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
        let outcome = manager
            .ingest(&spawn_corpus, with_ocr, Some(progress))
            .await;
        match &outcome {
            Ok(stats) => tracing::info!(
                corpus_id = %spawn_corpus,
                job_id = %spawn_job,
                files_indexed = stats.files_indexed,
                chunks_written = stats.chunks_written,
                "lc_http: ingest job finished"
            ),
            Err(e) => tracing::warn!(
                corpus_id = %spawn_corpus,
                job_id = %spawn_job,
                "lc_http: ingest job failed: {e}"
            ),
        }
        // ONE recorder for both arms and both ingest sites: it writes the
        // terminal receipt the progress route serves, and stamps the
        // phase file Failed on the error arm so a poller stops spinning.
        crate::corpus_watch_http::record_ingest_outcome(
            &index_dir,
            &spawn_corpus,
            &spawn_job,
            outcome.as_ref().map_err(|e| format!("ingest: {e}")),
        );
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(IngestJobAck {
            progress_route: format!("/internal/corpus/local/{corpus_id}/ingest/progress"),
            corpus_id,
            job_id,
            ok: true,
        }),
    )
        .into_response())
}

/// GET /internal/corpus/local/{corpus}/ingest/progress — how far along
/// the ingest is, and what it indexed when it is done.
///
/// # Why this route exists rather than the watch-status one
///
/// The ack used to name `GET /internal/corpus/watch/status/{corpus}`, and
/// 069660fd9 measured that it cannot serve this arm on two counts. Its
/// handler requires a RECONCILABLE watched folder, so it 404s for exactly
/// the `DocumentFolder` / OCR corpora this ingest arm exists to serve.
/// And it answers `WatchedFolderStatus`, which carries no `IngestStats`,
/// while the contract the desktop's progress channel closes on is a
/// terminal frame carrying `files_indexed`. A caller polling it could
/// only have finished by fabricating counts (ARCH §18.3), which is why
/// the desktop arm did not cross on that batch.
///
/// This route requires only that the corpus be REGISTERED — the same
/// check `ingest` itself makes, and the widest one that is still true —
/// so every corpus kind the ingest accepts can be followed to the end.
///
/// A registered corpus that has never ingested answers `200` with both
/// fields null and `finished: false`. That is not a 404: the corpus
/// exists and the answer to "how far along" is "it has not started",
/// which a caller renders differently from "no such corpus".
async fn ingest_progress(_: LocalOnly, Path(corpus_id): Path<String>) -> Result<Response, Absence> {
    let manager = manager_or_503()?;
    if manager.get(&corpus_id).await.is_none() {
        return Ok(not_registered(&corpus_id));
    }
    let index_dir = manager.index_dir_root().join(&corpus_id);
    let state = corpus_engine::enrichment::state::EnrichmentStateFile::read(&index_dir)
        .ok()
        .flatten();
    let outcome = crate::corpus_watch_http::IngestOutcome::read(&index_dir);
    let finished = outcome.is_some();
    tracing::debug!(
        corpus_id = %corpus_id,
        finished,
        phase = ?state.as_ref().map(|s| s.phase),
        files_indexed = ?outcome.as_ref().and_then(|o| o.stats.as_ref()).map(|s| s.files_indexed),
        "lc_http: ingest progress served",
    );
    Ok(Json(IngestProgress {
        corpus_id,
        state,
        outcome,
        finished,
    })
    .into_response())
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's ONE local-corpus manager, or the named 503. Same
/// singleton `corpus_watch_http` reads — this file installs nothing.
fn manager_or_503() -> Result<Arc<LocalCorpusManager>, Absence> {
    watched_folder_runtime::manager()
        .ok_or_else(|| Absence::unavailable("local-corpus runtime not installed on this daemon"))
}

fn not_registered(corpus_id: &str) -> axum::response::Response {
    not_found(format!("corpus '{corpus_id}' is not registered locally"))
}

/// The one place this family logs a 500 before answering it. The envelope is
/// [`crate::http_response`]'s; only the log line is local.
fn log_and_500(msg: &str) -> axum::response::Response {
    tracing::warn!("lc_http: {msg}");
    internal_error(msg)
}

/// Keeps `LocalCorpusConfig` named in this file's public surface: the
/// list and get routes answer it, and a reader looking for "what shape
/// comes back" should find the type without leaving the module.
#[allow(dead_code)]
fn _answers_local_corpus_config(c: LocalCorpusConfig) -> LocalCorpusConfig {
    c
}
