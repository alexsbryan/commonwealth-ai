// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local corpus ingest lifecycle endpoints.
//!
//! Single-node operations: starting an install, observing progress,
//! pausing/cancelling, expanding scope, and querying canonical status.
//! These handlers do not coordinate across the mesh — collaborative
//! ingestion is in `corpus_collaborate`, the work-queue protocol is in
//! `corpus_queue`; spawning an install is `corpus_install`. Its helpers
//! `spawn_corpus_install` and `spawn_corpus_install_with_parameters` are also called by the
//! collaborate path on partition-receiver peers, so they must stay
//! `pub` and reachable through the module facade.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use corpus_index::ingest_port::daemon::IngestPort;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

use super::corpus_install::{
    install_status, spawn_corpus_install_outcome, InstallRequest, InstallResponse,
};
use super::ErrorBody;

/// POST /internal/corpus/install — start (or resume) a corpus ingest.
///
/// Thin entry point to [`CorpusEngine::ingest`]. Desktop's Tauri
/// `install_corpus` command and the daemon's auto-collaborate loop
/// both call this so there is exactly one place where an ingest gets
/// spawned on this node: the shared helper
/// [`spawn_corpus_install`]. That helper owns `active_ingests`
/// bookkeeping and the `corpus_progress` map, so the
/// `/internal/corpus/progress` route and the `/internal/corpus/cancel`
/// route have consistent views of what is running.
///
/// Idempotent: a second call while the same corpus is already in
/// `active_ingests` returns `spawned: false` without starting a new
/// task. That's the "dual-path guard" — clicking Install in Desktop
/// while the daemon is already working on this corpus just no-ops.
pub async fn corpus_install(
    State(state): State<AppState>,
    Json(req): Json<InstallRequest>,
) -> Result<Json<InstallResponse>, (StatusCode, Json<ErrorBody>)> {
    if state.inner.node.corpus_engine.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: crate::hosted_ingest::NO_INGEST.into(),
            }),
        ));
    }
    // Map the typed outcome to an HTTP status (`install_status`, shared with
    // the OICP route). A recipe that can't be resolved or parameters that
    // don't validate are real failures the caller must see (4xx) — NOT a
    // `spawned:false` masquerading as success behind a 200.
    let outcome = spawn_corpus_install_outcome(state, req.corpus_id.clone(), req.parameters).await;
    let spawned = install_status(outcome, &req.corpus_id)?;
    Ok(Json(InstallResponse {
        corpus_id: req.corpus_id,
        spawned,
    }))
}

/// GET /internal/corpus/progress — snapshot of the latest progress
/// event observed for every corpus currently in
/// `active_ingests`, plus any corpus whose terminal `Complete` event
/// has not yet been evicted by a subsequent install.
///
/// Clients poll this (the Desktop UI polls every ~500 ms while an
/// install is in-flight). The response is a map keyed by corpus id
/// for direct lookup; an empty object means nothing is currently
/// ingesting on this node.
pub async fn corpus_progress(State(state): State<AppState>) -> Json<ProgressSnapshotResponse> {
    let snapshot = state.inner.ingest.corpus_progress.read().await.clone();
    Json(ProgressSnapshotResponse { progress: snapshot })
}

/// GET /internal/corpus/canonical/{corpus_id} — stream the canonical
/// index directory for `corpus_id` as a tar+zstd archive.
///
/// Phase 6 of the resilience track: peers that need to sync a
/// canonical (because their own is missing, smaller, or
/// fingerprint-divergent) fetch this endpoint and unpack into a
/// fresh dir. The response carries the canonical's
/// `canonical_fingerprint` in an `X-Canonical-Fingerprint` header
/// so the receiver can validate before atomic rename.
///
/// Refused with `404 Not Found` when:
///   - The corpus engine isn't wired (Commonwealth-only deployments).
///   - No canonical for `corpus_id` exists at this node.
///   - The canonical's `query_sharing` flag is false (private
///     corpora — e.g. a personal codebase — never leave the host).
///
/// The streaming model uses `tokio::io::duplex`: a blocking task
/// produces the tar.zst into the sync end while the response body
/// pipes the async end to the client. Memory bound is the duplex
/// buffer (64 KB), not the canonical size — so a 12 GB Wikipedia
/// canonical streams without fitting in RAM.
pub async fn corpus_canonical_stream(
    State(state): State<AppState>,
    axum::extract::Path(corpus_id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::body::Body;
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "corpus engine not wired on this node"})),
        )
            .into_response();
    };

    // Resolve the canonical path. We use `canonical_path` (engine
    // helper) to centralise the layout convention rather than
    // hand-joining `index_dir.join(&corpus_id)`.
    let canonical_path = engine.canonical_path(&corpus_id);
    if !canonical_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("no canonical for '{corpus_id}' at this node"),
            })),
        )
            .into_response();
    }

    // Resolve the index info so we can:
    //   1. Refuse private corpora (query_sharing=false).
    //   2. Surface the fingerprint header for client-side validation.
    let info = match corpus_index::index::CorpusIndex::open(&canonical_path).await {
        Ok(idx) => match idx.info().await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(
                    corpus_id,
                    error = %e,
                    "corpus_canonical_stream: cannot read index info"
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("info: {e}")})),
                )
                    .into_response();
            }
        },
        Err(e) => {
            tracing::warn!(
                corpus_id,
                error = %e,
                "corpus_canonical_stream: cannot open canonical"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("open: {e}")})),
            )
                .into_response();
        }
    };

    if !info.query_sharing {
        // Private corpus — refuse cross-peer transfer the same way
        // `build_hosted_corpora` filters them out of the gossip
        // catalog. Without this gate a peer who knew the corpus_id
        // out-of-band could still pull.
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": format!(
                    "corpus '{corpus_id}' is not query-sharable; \
                     mesh sync is disabled"
                ),
            })),
        )
            .into_response();
    }

    // Snapshot what we'll send so the spawn_blocking task doesn't
    // need to hold an `Arc` to the index. The path is stable — even
    // if the canonical is concurrently rewritten, an in-flight tar
    // stream reads from a consistent set of LanceDB fragment files
    // (LanceDB's append-only fragment layout means a concurrent
    // write produces NEW fragment files; the tar reads the existing
    // set we resolved at open time).
    let path_for_pack = canonical_path.clone();
    let fp_header_value = info.canonical_fingerprint.clone().unwrap_or_default();
    let chunk_count_header = info.chunk_count;

    // Duplex pipe: blocking task writes tar.zst into the sync end;
    // the async end becomes the response body via ReaderStream.
    // 64 KiB matches axum's default streaming chunk; smaller buffers
    // cost more syscalls, larger ones don't help on most networks.
    let (async_writer, async_reader) = tokio::io::duplex(64 * 1024);
    let sync_writer = tokio_util::io::SyncIoBridge::new(async_writer);

    let engine_for_pack = engine.clone();
    tokio::task::spawn_blocking(move || {
        // Compression level 1 — fast on the sender, ~10% larger than
        // default (3) in our benchmarks. We're network-bound on the
        // common LAN/WAN case; the receiver wins more from sooner-
        // available bytes than from smaller transfer.
        match engine_for_pack.pack_canonical(&path_for_pack, Box::new(sync_writer), 1) {
            Ok(bytes_in) => {
                tracing::info!(
                    corpus = path_for_pack
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?"),
                    bytes_in,
                    "corpus_canonical_stream: pack complete"
                );
            }
            Err(e) => {
                // The duplex sync end will close when this fn
                // returns; the client sees an early EOF + the
                // tar/zstd parser errors at the receiver. We can't
                // surface a structured error mid-stream over plain
                // HTTP body, but the warn log + receiver-side
                // fingerprint validation gives operators enough to
                // diagnose.
                tracing::warn!(
                    corpus = path_for_pack.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    error = %e,
                    "corpus_canonical_stream: pack failed mid-stream"
                );
            }
        }
    });

    let body_stream = tokio_util::io::ReaderStream::new(async_reader);

    let mut resp = axum::response::Response::new(Body::from_stream(body_stream));
    *resp.status_mut() = StatusCode::OK;
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/x-tar+zstd"),
    );
    if !fp_header_value.is_empty() {
        if let Ok(v) = fp_header_value.parse() {
            headers.insert("x-canonical-fingerprint", v);
        }
    }
    if let Ok(v) = chunk_count_header.to_string().parse() {
        headers.insert("x-canonical-chunk-count", v);
    }
    resp
}

/// GET /internal/corpus/status — richer per-corpus snapshot that
/// combines every signal the Desktop UI needs to render the
/// "Installing…" row without needing to have initiated the install
/// itself.
///
/// Reports an entry for every corpus where any of:
///   - an ingest task is currently in `active_ingests`;
///   - a canonical or partition-of-self directory is present with
///     `ingestion_in_progress=true` (daemon-owned resume after a
///     Desktop close / crash);
///   - a recent progress event is cached but the task has already
///     exited (so terminal phases still propagate to a late
///     subscriber).
///
/// Each entry fuses the latest `IngestProgress` with on-disk state
/// (shard counts, committed_iter_pos, partition/canonical presence)
/// plus a best-effort `estimated_fraction`. The Desktop poller reads
/// this and emits `corpus-progress` events so the UI state stays in
/// sync whether or not this particular Desktop session kicked off
/// the install.
pub async fn corpus_status(State(state): State<AppState>) -> Json<CorpusStatusResponse> {
    let engine = match state.inner.node.corpus_engine.as_ref() {
        Some(e) => e.clone(),
        None => {
            return Json(CorpusStatusResponse {
                entries: Vec::new(),
            });
        }
    };

    // Union of every corpus id worth reporting. Using a BTreeSet so
    // the response is deterministically ordered — makes debugging
    // and the integration test's snapshot comparisons less flaky.
    let mut candidates: std::collections::BTreeSet<String> = Default::default();
    for id in state.inner.ingest.active_ingests.read().await.iter() {
        candidates.insert(id.clone());
    }
    for id in state.inner.ingest.corpus_progress.read().await.keys() {
        candidates.insert(id.clone());
    }
    candidates.extend(engine.in_progress_ingestions());

    let active_snapshot = state.inner.ingest.active_ingests.read().await.clone();
    let progress_snapshot = state.inner.ingest.corpus_progress.read().await.clone();

    // Gather per-corpus data, then spawn sample jobs for any corpus
    // that needs a fresh article-stats sidecar. We do this OFF the
    // async runtime (`spawn_blocking`) because the first sample for
    // a ~74 GB Wikipedia JSONL burns 1–2 s of synchronous I/O;
    // doing it inline would block other handlers on this axum worker.
    let mut entries: Vec<CorpusStatusEntry> = Vec::new();
    for corpus_id in candidates {
        let disk = engine.corpus_disk_status(&corpus_id);
        let active = active_snapshot.contains(&corpus_id);
        let progress = progress_snapshot.get(&corpus_id).cloned();
        // Cheap sidecar read — no I/O beyond a small file if it
        // exists. Sidecar is absent on the first daemon-session
        // observation of a corpus; we kick off the sampler below and
        // the next `/status` poll will pick up the fresh value.
        let cached_stats = engine.cached_article_stats(&corpus_id);

        if cached_stats.is_none() && disk.committed_iter_pos > 0 {
            // Spawn the sampler in the background. It writes the
            // sidecar on completion; the next poll reads it.
            let engine_for_task = engine.clone();
            let corpus_id_for_task = corpus_id.clone();
            tokio::task::spawn_blocking(move || {
                let _ = engine_for_task.compute_article_stats(&corpus_id_for_task);
            });
        }

        let estimated_fraction = disk
            .estimated_fraction()
            .or_else(|| {
                // Sample-derived fraction for the legacy / resume
                // path: committed sections vs estimated total.
                let stats = cached_stats.as_ref()?;
                if stats.total_sections_estimate == 0 {
                    return None;
                }
                Some(
                    (disk.committed_iter_pos as f32 / stats.total_sections_estimate as f32)
                        .clamp(0.0, 1.0),
                )
            })
            .or_else(|| progress.as_ref().and_then(progress_fraction));

        entries.push(CorpusStatusEntry {
            corpus_id: corpus_id.clone(),
            active,
            progress,
            shards_completed: disk.shards_completed.len(),
            shards_total: disk.shards_total,
            committed_iter_pos: disk.committed_iter_pos,
            canonical_present: disk.canonical_present,
            partition_present: disk.partition_present,
            canonical_in_progress: disk.canonical_in_progress,
            partition_in_progress: disk.partition_in_progress,
            estimated_fraction,
            estimated_total_sections: cached_stats.as_ref().map(|s| s.total_sections_estimate),
            estimated_total_articles: cached_stats.as_ref().map(|s| s.total_articles),
        });
    }

    Json(CorpusStatusResponse { entries })
}

pub(crate) fn progress_fraction(
    progress: &sovereign_contracts::daemon_wire::IngestProgress,
) -> Option<f32> {
    use sovereign_contracts::daemon_wire::IngestProgress as P;
    match progress {
        P::Downloading { percent, .. } => Some((*percent / 100.0).clamp(0.0, 1.0)),
        P::Embedding {
            chunks_embedded,
            total,
            ..
        } if *total > 0 => Some(((*chunks_embedded as f32) / (*total as f32)).clamp(0.0, 1.0)),
        P::Indexing {
            chunks_indexed,
            total,
        } if *total > 0 => Some(((*chunks_indexed as f32) / (*total as f32)).clamp(0.0, 1.0)),
        // Rebuild is one-shot — show as in-flight (0.5) so the bar
        // doesn't snap from full back to empty between Indexing and
        // Complete during an expansion.
        P::OptimizingIndex { .. } => Some(0.5),
        // Enrichment phase events surface a sub-fraction when the
        // underlying phase reports one (Phase 1b batches, clustering
        // milestone). Otherwise we leave it None — the desktop falls
        // back to the per-phase label rather than rendering a static
        // bar position.
        P::Enriching { fraction, .. } => *fraction,
        P::Complete { .. } => Some(1.0),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
pub struct CorpusStatusResponse {
    pub entries: Vec<CorpusStatusEntry>,
}

#[derive(Debug, Serialize)]
pub struct CorpusStatusEntry {
    pub corpus_id: String,
    /// A task is currently tracked in `active_ingests` for this
    /// corpus. False means either no ingest is running, or an
    /// ingest exited without clearing its entry (daemon crash).
    pub active: bool,
    /// Latest `IngestProgress` observed for this corpus, if any.
    pub progress: Option<sovereign_contracts::daemon_wire::IngestProgress>,
    pub shards_completed: usize,
    pub shards_total: usize,
    pub committed_iter_pos: u64,
    pub canonical_present: bool,
    pub partition_present: bool,
    pub canonical_in_progress: bool,
    pub partition_in_progress: bool,
    /// Best-effort completion fraction in `[0.0, 1.0]`. `None` when
    /// we genuinely can't estimate (e.g. pre-first-embed-batch in
    /// a legacy canonical resume where shards aren't tracked).
    pub estimated_fraction: Option<f32>,
    /// Cached sample estimate of total sections (extractor-emitted
    /// documents) in the source JSONL. Drives the resume-path
    /// percent via `committed_iter_pos / total`. `None` until the
    /// sampler has written a sidecar for this corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_total_sections: Option<u64>,
    /// Cached sample estimate of total JSONL lines (articles) in
    /// the source. Exposed mainly for diagnostic display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_total_articles: Option<u64>,
}

/// Spawn an `engine.ingest` task for `corpus_id`, unifying the
/// lifecycle bookkeeping across every entry point (install route,
/// auto-collaborate loop, future CLI).
///
/// Responsibilities kept in this one place:
///   - Idempotency guard: skip spawn when `corpus_id` is already in
///     `active_ingests`. Returns `false` so the caller can surface
///     "already ingesting" to the user.
///   - `active_ingests` insert / remove around the spawn.
///   - `corpus_progress` map updates via a progress callback that
///     writes on every `IngestProgress` event.
///   - Result logging with `Error::Cancelled` treated as a clean
///     outcome (the `/internal/corpus/cancel` route has already
///     wiped the partition when this returns).
///
/// Returns `true` when a new task was spawned, `false` when a task
/// was already live for this corpus.
/// POST /internal/corpus/expand — relax the active filter scope on an
/// installed corpus (e.g. promote Wikipedia from Core to Full) by
/// running [`corpus_engine::CorpusEngine::expand_corpus`] in the
/// background. Progress streams on the same `corpus-progress` channel
/// the install path uses, with phase strings the Desktop poller
/// already forwards verbatim.
///
/// Idempotent at the `active_ingests` layer: a second call while an
/// expansion is already in flight returns `spawned: false`.
pub async fn corpus_expand(
    State(state): State<AppState>,
    Json(req): Json<ExpandRequest>,
) -> Result<Json<InstallResponse>, (StatusCode, Json<ErrorBody>)> {
    if state.inner.node.corpus_engine.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: crate::hosted_ingest::NO_INGEST.into(),
            }),
        ));
    }
    let spawned = spawn_corpus_expand(state, req.corpus_id.clone()).await;
    Ok(Json(InstallResponse {
        corpus_id: req.corpus_id,
        spawned,
    }))
}

/// Spawn an expand task that calls
/// [`corpus_engine::CorpusEngine::expand_corpus_to_full`] in the
/// background. Mirrors [`spawn_corpus_install`]'s lifecycle so the
/// existing status / progress / cancel plumbing works unchanged.
// ─── Terminal-outcome bookkeeping, shared by install and expand ──────
//
// Install and expand run the same status/progress/poller pipeline, so
// they must agree on how a terminal outcome is recorded. These three
// helpers are that agreement in one place; duplicating them is how the
// expand path came to swallow its failures while install reported them.
// The governing invariant is documented on `IngestProgress::Failed`.

/// The progress callback both spawn paths install into the engine.
///
/// Latest-wins per corpus, with one exception: a terminal `Failed`
/// record is never overwritten. Each insert runs in its own spawned
/// task, so ordering against the failure write is NOT guaranteed —
/// and losing that race would park the corpus on a non-terminal phase
/// (say "embedding") with no task running to ever advance it, i.e. a
/// permanent fake spinner in place of the error. Safe across retries
/// because `clear_stale_failure` runs before a new attempt spawns.
pub(super) fn ingest_progress_callback(
    state: AppState,
    corpus_id: String,
) -> corpus_index::ingest_port::ProgressCallback {
    Box::new(move |p| {
        let state = state.clone();
        let corpus_id = corpus_id.clone();
        // The callback is synchronous but the map needs an async lock.
        // Spawn a short-lived task; it finishes essentially instantly.
        tokio::spawn(async move {
            let mut map = state.inner.ingest.corpus_progress.write().await;
            if matches!(
                map.get(&corpus_id),
                Some(sovereign_contracts::daemon_wire::IngestProgress::Failed { .. })
            ) {
                return;
            }
            map.insert(corpus_id, p);
        });
    })
}

/// Retire a `Failed` record left by a previous attempt, so a retry
/// starts clean.
///
/// The failure record is deliberately sticky — it has to outlive its
/// task to be reportable at all — which makes the retry responsible for
/// clearing it. Without this, the stale message would sit in the
/// snapshot beside live progress and a UI showing "Install failed" would
/// keep showing it straight through a successful reinstall.
///
/// Only `Failed` is swept: in-flight phases cannot be present (the
/// `active_ingests` guard already returned), and a `Complete` entry is
/// legitimate history until overwritten.
pub(super) async fn clear_stale_failure(state: &AppState, corpus_id: &str) {
    let mut progress = state.inner.ingest.corpus_progress.write().await;
    if let Some(sovereign_contracts::daemon_wire::IngestProgress::Failed { .. }) =
        progress.get(corpus_id)
    {
        progress.remove(corpus_id);
    }
}

/// Record a terminal failure so `/internal/corpus/status` can report it.
///
/// Not merely a log line: `active_ingests` has already dropped this
/// corpus by the time we get here, and `corpus_status` builds its
/// candidate set from `active_ingests ∪ corpus_progress`. With no entry
/// the corpus vanishes from the response, and the Desktop poller reads
/// "present last tick, absent this tick" as SUCCESS — emitting
/// phase=complete / 100% / "Done" for an install that committed nothing.
pub(super) async fn record_failure(state: &AppState, corpus_id: &str, message: String) {
    state.inner.ingest.corpus_progress.write().await.insert(
        corpus_id.to_string(),
        sovereign_contracts::daemon_wire::IngestProgress::Failed { message },
    );
}

pub async fn spawn_corpus_expand(state: AppState, corpus_id: String) -> bool {
    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        return false;
    };

    {
        let mut active = state.inner.ingest.active_ingests.write().await;
        if active.contains(&corpus_id) {
            return false;
        }
        active.insert(corpus_id.clone());
    }

    clear_stale_failure(&state, &corpus_id).await;

    let state_for_task = state.clone();
    let corpus_id_for_task = corpus_id.clone();
    tokio::spawn(async move {
        let progress_cb =
            ingest_progress_callback(state_for_task.clone(), corpus_id_for_task.clone());

        let result = engine
            .expand_corpus_to_full(&corpus_id_for_task, Some(progress_cb))
            .await;

        state_for_task
            .inner
            .ingest
            .active_ingests
            .write()
            .await
            .remove(&corpus_id_for_task);

        match result {
            Ok(info) => tracing::info!(
                corpus = %corpus_id_for_task,
                chunks = info.chunks_created,
                "spawn_corpus_expand: expansion complete"
            ),
            Err(e) => {
                // Same contract as the install path: a terminal failure
                // is RECORDED, not merely logged. Expansion shares the
                // whole status/progress/poller pipeline, so a log-only
                // handler here reproduces the identical bug.
                record_failure(&state_for_task, &corpus_id_for_task, e.to_string()).await;
                tracing::warn!(
                    corpus = %corpus_id_for_task,
                    error = %e,
                    "spawn_corpus_expand: expansion failed"
                );
            }
        }
    });
    true
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExpandRequest {
    pub corpus_id: String,
}

#[derive(Debug, Serialize)]
pub struct ProgressSnapshotResponse {
    pub progress:
        std::collections::HashMap<String, sovereign_contracts::daemon_wire::IngestProgress>,
}

/// Signal the corpus's cancellation flag and wait (bounded) for the
/// in-flight ingest task to exit. Shared between `/pause` and `/cancel`
/// — both want a clean stop before they decide what to do with on-disk
/// state.
///
/// Returns whether a live task was actually signalled. After this
/// helper returns the corpus is no longer in `active_ingests` (or the
/// 5 s ceiling was hit and we've logged a warning).
async fn stop_in_flight_ingest(state: &AppState, engine: &dyn IngestPort, corpus_id: &str) -> bool {
    let cancelled = engine.cancel_corpus_ingest(corpus_id);

    // Bounded poll until the spawn clears from active_ingests. We do
    // this via polling rather than a notify because active_ingests is
    // mutated from multiple task sites (collaborate spawn, peer
    // partition spawn, future install command) — a single Notify would
    // need to be fired from every one of them and we'd miss races.
    // 5 s is generous: the ingest loop polls cancel between each doc
    // and between every tier-2 flush (~60 s of work max), but each
    // individual doc takes milliseconds, so the loop exits promptly
    // in practice. The wait only hits the ceiling when cancel is
    // fired during a slow embed call that can't be interrupted.
    if cancelled {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let still_active = state
                .inner
                .ingest
                .active_ingests
                .read()
                .await
                .contains(corpus_id);
            if !still_active {
                break;
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    corpus = %corpus_id,
                    "stop_in_flight_ingest: task did not exit within 5s"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // Drop the progress entry so polling clients see "not_installed"
    // on their next tick instead of a stale final-embedding frame.
    state
        .inner
        .ingest
        .corpus_progress
        .write()
        .await
        .remove(corpus_id);

    cancelled
}

/// POST /internal/corpus/pause — non-destructive stop.
///
/// Signals the corpus's cancellation flag and waits for the in-flight
/// ingest task to exit cleanly, but **does not** wipe on-disk state.
/// `_corpus_meta.json` keeps its `committed_iter_pos`; chunks.lance
/// keeps every flushed shard. To resume, POST /internal/corpus/install
/// again — the loop reads existing meta and skips past committed docs.
///
/// This is the safe default for "user clicked Cancel during an
/// in-progress ingest." For the destructive variant (delete everything
/// for this corpus on this node), see /internal/corpus/cancel.
///
/// Returns 200 even when no ingest is active — useful for idempotent
/// "make sure nothing is running" calls.
pub async fn corpus_pause(
    State(state): State<AppState>,
    Json(req): Json<CancelRequest>,
) -> Result<Json<PauseResponse>, (StatusCode, Json<ErrorBody>)> {
    let engine = state.inner.node.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: crate::hosted_ingest::NO_INGEST.into(),
            }),
        )
    })?;

    let cancelled = stop_in_flight_ingest(&state, engine.as_ref(), &req.corpus_id).await;

    tracing::info!(
        corpus = %req.corpus_id,
        cancel_signalled = cancelled,
        "corpus_pause: ingest stopped, on-disk state preserved"
    );

    Ok(Json(PauseResponse {
        cancel_signalled: cancelled,
    }))
}

/// POST /internal/corpus/cancel — destructive stop + wipe.
///
/// Requires `confirm_wipe: true` in the request body. Without it the
/// route returns 400 — the prior implicit-wipe behaviour caused
/// accidental loss of weeks of ingest work and the explicit confirm
/// is the guardrail against repeating that. For a non-destructive
/// stop, POST /internal/corpus/pause instead.
///
/// Flow:
///   1. Fire the corpus's cancellation flag via the engine's registry.
///      The ingest loop polls this flag at every document + flush
///      boundary and exits with `Error::Cancelled` at the next safe
///      point, without corrupting LanceDB.
///   2. Wait (bounded, ~5 s) for the spawn to clear out of
///      `active_ingests` so that no concurrent writer is left behind
///      when we wipe the directories.
///   3. Wipe canonical `<corpus>/` and every `<corpus>-partition-*/`
///      sibling via `engine.remove_corpus_everything`. Peers' own
///      partition dirs on other machines are not affected (per the
///      "cancel is local" decision in the unified-ingest plan).
///
/// Returns 200 even when no ingest was active for this corpus — the
/// wipe still runs, so a stale partition dir left over from a crashed
/// earlier session gets cleaned up too. The response carries whether a
/// cancel signal was actually delivered so callers can distinguish
/// "cancelled a live ingest" from "idempotent cleanup".
pub async fn corpus_cancel(
    State(state): State<AppState>,
    Json(req): Json<CancelRequest>,
) -> Result<Json<CancelResponse>, (StatusCode, Json<ErrorBody>)> {
    if !req.confirm_wipe.unwrap_or(false) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: "/internal/corpus/cancel is destructive and requires \
                    `confirm_wipe: true`. To stop without wiping, use \
                    /internal/corpus/pause instead."
                    .into(),
            }),
        ));
    }

    let engine = state.inner.node.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: crate::hosted_ingest::NO_INGEST.into(),
            }),
        )
    })?;

    let cancelled = stop_in_flight_ingest(&state, engine.as_ref(), &req.corpus_id).await;

    // Wipe canonical + every partition-* sibling for this corpus.
    if let Err(e) = engine.remove_corpus_everything(&req.corpus_id) {
        tracing::warn!(
            corpus = %req.corpus_id,
            error = %e,
            "corpus_cancel: wipe reported an error; returning failure to caller"
        );
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("failed to wipe corpus '{}': {e}", req.corpus_id),
            }),
        ));
    }

    tracing::info!(
        corpus = %req.corpus_id,
        cancel_signalled = cancelled,
        "corpus_cancel: cleanup complete"
    );

    Ok(Json(CancelResponse {
        cancel_signalled: cancelled,
        wiped: true,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    pub corpus_id: String,
    /// Required for `/internal/corpus/cancel` to perform the destructive
    /// wipe. Ignored by `/internal/corpus/pause`. Optional in the wire
    /// format so missing-field errors surface as a 400 with a helpful
    /// message rather than a generic deserialisation error.
    #[serde(default)]
    pub confirm_wipe: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CancelResponse {
    /// True when a live ingest task for this corpus existed and was
    /// signalled to stop. False for an idempotent cleanup call (no
    /// task was running).
    pub cancel_signalled: bool,
    /// True when the on-disk wipe completed without error.
    pub wiped: bool,
}

#[derive(Debug, Serialize)]
pub struct PauseResponse {
    /// True when a live ingest task for this corpus existed and was
    /// signalled to stop. False when no task was running (idempotent).
    pub cancel_signalled: bool,
}
