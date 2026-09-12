// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local corpus ingest lifecycle endpoints.
//!
//! Single-node operations: starting an install, observing progress,
//! pausing/cancelling, expanding scope, and querying canonical status.
//! These handlers do not coordinate across the mesh — collaborative
//! ingestion is in `corpus_collaborate`, the work-queue protocol is in
//! `corpus_queue`. Helpers `spawn_corpus_install` and
//! `spawn_corpus_install_with_parameters` are also called by the
//! collaborate path on partition-receiver peers, so they must stay
//! `pub` and reachable through the module facade.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use commonwealth_core::activity::ActivityEventKind;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

use super::ErrorBody;

/// Resolve the CLI binary that can actually run `enrich init/extract`
/// for the deep Tier-2 referential-atlas pass.
///
/// `enrich` is owned ONLY by `sovereign-cli-llm`. **No daemon process
/// is that binary**, so we must NEVER self-exec `std::env::current_exe()`
/// here (the historical bug):
/// - Standalone daemon → `current_exe()` is `sovereign-cli-daemon`,
///   whose dispatcher rejects `enrich` (exit 2) — a dead pass.
/// - Desktop embedded in-process daemon → `current_exe()` is the Tauri
///   GUI binary (`sovereign-desktop`), which has no arg-parser and no
///   single-instance guard: self-execing it **mis-launches a second GUI
///   window** and blocks this post-install task on it.
///
/// So the deep pass is opt-in via an explicit `$SOVEREIGN_CLI_LLM_BIN`
/// pointing at a real `sovereign-cli-llm`. When it is unset (every
/// default deployment) we skip — the structural atlas + inline tiered
/// enrichment have already landed by this point, so the referential
/// atlas is an optional publisher-side deepening, not a prerequisite for
/// retrieval or Explore. We deliberately do NOT auto-discover a sibling
/// binary or fall back to `which`: that would silently turn on a heavy
/// LLM pass on the standalone daemon (which has never run it), changing
/// behaviour no operator asked for.
fn resolve_enrich_cli() -> Option<std::path::PathBuf> {
    let raw = std::env::var("SOVEREIGN_CLI_LLM_BIN").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(trimmed);
    if path.exists() {
        Some(path)
    } else {
        tracing::warn!(
            configured = %path.display(),
            "SOVEREIGN_CLI_LLM_BIN is set but the path does not exist — skipping tier-2 deep extraction"
        );
        None
    }
}

/// Phase C3 — gather peer atlas advice for `corpus_id`.
///
/// Walks the live mesh, builds [`PeerAtlasView`]s from each peer's
/// `hosted_corpora`, reads the local atlas summary, and returns the
/// best pull candidate (if any) per the rule in
/// [`evaluate_peer_atlas_advice`].
///
/// Returns `None` when no peer is worth pulling from — the post-
/// install hook then proceeds with the local Tier-2 launch as
/// usual. Best-effort: any I/O hiccup falls through to "no advice"
/// rather than blocking the install.
async fn gather_peer_atlas_advice(
    state: &AppState,
    corpus_id: &str,
    indexes_dir: &std::path::Path,
) -> Option<sovereign_tools::atlas_peer_advice::PeerAtlasPullCandidate> {
    use sovereign_tools::atlas_peer_advice::{evaluate_peer_atlas_advice, PeerAtlasView};

    // Local view: atom counts come from the cached summary; embed
    // model from our own member record (populated by gossip).
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    let local_summary = corpus_engine::enrichment::atlas::read_or_compute_atlas_summary(&atlas_dir)
        .ok()
        .flatten();
    let local_tier2_count = local_summary.as_ref().map(|s| s.tier2_count).unwrap_or(0);
    let local_fingerprint = local_summary.as_ref().map(|s| s.fingerprint.as_str());

    let self_node_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let mesh = state.inner.mesh.read().await;
    let my_embed_model = mesh
        .members
        .get(&self_node_id)
        .and_then(|m| m.capabilities.embed_model.as_ref())
        .map(|m| m.model_id.clone());

    let mut peer_views: Vec<PeerAtlasView> = Vec::new();
    for (node_id, member) in mesh.members.iter() {
        if *node_id == self_node_id {
            continue;
        }
        let model = member
            .capabilities
            .embed_model
            .as_ref()
            .map(|m| m.model_id.clone());
        if let Some(view) = PeerAtlasView::from_member(
            member.name.clone(),
            model,
            corpus_id,
            &member.capabilities.hosted_corpora,
        ) {
            peer_views.push(view);
        }
    }

    evaluate_peer_atlas_advice(
        local_tier2_count,
        local_fingerprint,
        my_embed_model.as_deref(),
        &peer_views,
    )
}

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
    if state.inner.corpus_engine.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        ));
    }
    // Map the typed outcome to an HTTP status. A recipe that can't be
    // resolved or parameters that don't validate are real failures the
    // caller must see (4xx) — NOT a `spawned:false` masquerading as
    // success behind a 200. Only "already in flight" is a benign no-op.
    match spawn_corpus_install_outcome(state, req.corpus_id.clone(), req.parameters).await {
        InstallOutcome::Spawned => Ok(Json(InstallResponse {
            corpus_id: req.corpus_id,
            spawned: true,
        })),
        InstallOutcome::AlreadyActive => Ok(Json(InstallResponse {
            corpus_id: req.corpus_id,
            spawned: false,
        })),
        InstallOutcome::NoEngine => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        )),
        InstallOutcome::RecipeNotFound(reason) => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("cannot install '{}': {reason}", req.corpus_id),
            }),
        )),
        InstallOutcome::InvalidParameters(reason) => Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("invalid parameters for '{}': {reason}", req.corpus_id),
            }),
        )),
    }
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
    let snapshot = state.inner.corpus_progress.read().await.clone();
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

    let Some(engine) = state.inner.corpus_engine.clone() else {
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
    let info = match corpus_engine::index::CorpusIndex::open(&canonical_path).await {
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

    tokio::task::spawn_blocking(move || {
        // Compression level 1 — fast on the sender, ~10% larger than
        // default (3) in our benchmarks. We're network-bound on the
        // common LAN/WAN case; the receiver wins more from sooner-
        // available bytes than from smaller transfer.
        match corpus_engine::canonical_sync::pack_canonical(&path_for_pack, sync_writer, 1) {
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
    let engine = match state.inner.corpus_engine.as_ref() {
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
    for id in state.inner.active_ingests.read().await.iter() {
        candidates.insert(id.clone());
    }
    for id in state.inner.corpus_progress.read().await.keys() {
        candidates.insert(id.clone());
    }
    candidates.extend(engine.in_progress_ingestions());

    let active_snapshot = state.inner.active_ingests.read().await.clone();
    let progress_snapshot = state.inner.corpus_progress.read().await.clone();

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

pub(crate) fn progress_fraction(progress: &corpus_engine::IngestProgress) -> Option<f32> {
    use corpus_engine::IngestProgress as P;
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
    pub progress: Option<corpus_engine::IngestProgress>,
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
    if state.inner.corpus_engine.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
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
fn ingest_progress_callback(state: AppState, corpus_id: String) -> corpus_engine::ProgressCallback {
    Box::new(move |p| {
        let state = state.clone();
        let corpus_id = corpus_id.clone();
        // The callback is synchronous but the map needs an async lock.
        // Spawn a short-lived task; it finishes essentially instantly.
        tokio::spawn(async move {
            let mut map = state.inner.corpus_progress.write().await;
            if matches!(
                map.get(&corpus_id),
                Some(corpus_engine::IngestProgress::Failed { .. })
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
async fn clear_stale_failure(state: &AppState, corpus_id: &str) {
    let mut progress = state.inner.corpus_progress.write().await;
    if let Some(corpus_engine::IngestProgress::Failed { .. }) = progress.get(corpus_id) {
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
async fn record_failure(state: &AppState, corpus_id: &str, message: String) {
    state.inner.corpus_progress.write().await.insert(
        corpus_id.to_string(),
        corpus_engine::IngestProgress::Failed { message },
    );
}

pub async fn spawn_corpus_expand(state: AppState, corpus_id: String) -> bool {
    let Some(engine) = state.inner.corpus_engine.clone() else {
        return false;
    };

    {
        let mut active = state.inner.active_ingests.write().await;
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

pub async fn spawn_corpus_install(state: AppState, corpus_id: String) -> bool {
    spawn_corpus_install_with_parameters(state, corpus_id, std::collections::BTreeMap::new()).await
}

/// Like [`spawn_corpus_install`] but threads recipe parameters and
/// returns the full [`InstallOutcome`] instead of a bool — the HTTP
/// handler needs to distinguish a failure from a benign no-op. Most
/// callers want the bool projection [`spawn_corpus_install_with_parameters`].
///
/// The recipe is fetched up front, its parameter schema validated via
/// [`Recipe::resolve_parameters`], and the stamped recipe passed to
/// `engine.ingest` via [`CorpusSpec::Inline`] so the runtime carries the
/// resolved values into `http_api` URL/body interpolation. Mismatched /
/// missing parameters — and an unresolvable recipe — surface as a
/// typed failure here, *before* the background task spawns, so the
/// caller sees a 4xx on the install POST instead of a silent "ingest
/// failed" three minutes later.
pub async fn spawn_corpus_install_outcome(
    state: AppState,
    corpus_id: String,
    parameters: std::collections::BTreeMap<String, serde_json::Value>,
) -> InstallOutcome {
    let Some(engine) = state.inner.corpus_engine.clone() else {
        tracing::warn!(
            corpus = %corpus_id,
            "spawn_corpus_install: no corpus engine — ignoring"
        );
        return InstallOutcome::NoEngine;
    };

    {
        let mut active = state.inner.active_ingests.write().await;
        if active.contains(&corpus_id) {
            tracing::info!(
                corpus = %corpus_id,
                "spawn_corpus_install: already active — not spawning a second task"
            );
            return InstallOutcome::AlreadyActive;
        }
        active.insert(corpus_id.clone());
    }

    clear_stale_failure(&state, &corpus_id).await;

    // Resolve the recipe + apply parameters BEFORE spawning the
    // background task so a parameter mismatch surfaces as a
    // synchronous failure instead of a silent crash later.
    let recipe = match engine.registry().fetch_recipe(&corpus_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                corpus = %corpus_id,
                error = %e,
                "spawn_corpus_install: recipe fetch failed"
            );
            // Roll back the active_ingests insert so a subsequent
            // retry isn't blocked.
            state.inner.active_ingests.write().await.remove(&corpus_id);
            return InstallOutcome::RecipeNotFound(e.to_string());
        }
    };

    // Convert the JSON parameter map into TOML values so the
    // recipe's resolve_parameters can validate them against the
    // declared schema. JSON arrays of strings become TOML arrays;
    // JSON strings stay strings. We don't try to be clever: the
    // CLI / desktop already shaped the input.
    let toml_params = match json_params_to_toml(&parameters) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                corpus = %corpus_id,
                error = %e,
                "spawn_corpus_install: parameter coercion failed"
            );
            state.inner.active_ingests.write().await.remove(&corpus_id);
            return InstallOutcome::InvalidParameters(e);
        }
    };
    let resolved = match recipe.resolve_parameters(&toml_params) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                corpus = %corpus_id,
                error = %e,
                "spawn_corpus_install: parameter validation failed"
            );
            state.inner.active_ingests.write().await.remove(&corpus_id);
            return InstallOutcome::InvalidParameters(e.to_string());
        }
    };
    let recipe = recipe.with_resolved_parameters(resolved);

    let state_for_task = state.clone();
    let corpus_id_for_task = corpus_id.clone();
    tokio::spawn(async move {
        // Progress callback: latest-wins per corpus, except that a
        // terminal failure is never clobbered. See
        // `ingest_progress_callback`.
        let progress_cb =
            ingest_progress_callback(state_for_task.clone(), corpus_id_for_task.clone());

        // Respect a recipe's explicit retrieval-only opt-out: a recipe with
        // `[enrichment] enabled = false` skips the default post-install
        // structural-atlas + Tier-2 RAPTOR pass below, keeping retrieval sealed
        // to its own chunks (e.g. the chaos-monkey bench corpus). A recipe with
        // NO [enrichment] keeps the default-on hook. Computed here because
        // `recipe` is moved into the CorpusSpec on the next line.
        let recipe_opts_out_of_auto_enrichment = recipe.opts_out_of_auto_enrichment();
        let spec = corpus_engine::CorpusSpec::Inline(Box::new(recipe));
        let result = engine.ingest(&spec, Some(progress_cb)).await;

        state_for_task
            .inner
            .active_ingests
            .write()
            .await
            .remove(&corpus_id_for_task);

        match result {
            Ok(info) => {
                tracing::info!(
                    corpus = %corpus_id_for_task,
                    chunks = info.chunks_created,
                    duration_secs = info.duration_secs,
                    "spawn_corpus_install: ingest complete"
                );
                // Record the ingest on the local Activity ledger — the
                // headline "your import did real work" signal. Embedding
                // thousands of chunks is heavy local resource use that
                // never crosses a peer boundary, so the contribution
                // ledger never sees it; this is where it becomes visible.
                state_for_task
                    .inner
                    .activity_emitter
                    .record(ActivityEventKind::ChunksIngested {
                        corpus_id: corpus_id_for_task.clone(),
                        chunks: info.chunks_created,
                        duration_secs: info.duration_secs,
                    });
                // Post-install hook: build the structural atlas the
                // moment chunks are committed. Detached so the route
                // handler that triggered the install isn't held up
                // by the atlas pass; idempotent — a re-install or
                // restart is a no-op once `atlas/atoms.json` exists.
                let cid = corpus_id_for_task.clone();
                // structure_first doesn't read recipes — it walks chunks
                // by metadata. Pass the same path for both to satisfy
                // the CorpusEngine constructor without a recipe lookup.
                let indexes = engine.index_dir().to_path_buf();
                let recipes = indexes.clone();
                let enrich_activity = state_for_task.inner.activity_emitter.clone();
                // The SEC filings corpus's typed fact store moves into
                // the index dir HERE, synchronously, BEFORE the detached
                // block below. The `sec_facts` tool resolves a corpus by
                // the presence of that store, so any window where the
                // corpus is installed and the store is not yet placed is
                // a window where the tool reports "no installed SEC
                // corpus" for a corpus the user just watched install.
                // The atlas pass below is detached because it is
                // expensive; a file copy is not. No-op for every corpus
                // that has no staged store.
                if let Err(e) = sovereign_tools::sec_edgar::install_fact_sidecar(&cid, &indexes) {
                    tracing::warn!(
                        corpus = %cid, error = %e,
                        "post-install: typed fact store could not be placed — financial \
                         figures will refuse for this corpus until it is"
                    );
                }
                tokio::spawn(async move {
                    // Recipe opted out of auto-enrichment (retrieval-only):
                    // skip the structural-atlas + Tier-2 RAPTOR pass entirely
                    // so retrieval stays sealed to the source chunks.
                    if recipe_opts_out_of_auto_enrichment {
                        tracing::info!(
                            corpus = %cid,
                            "post-install: recipe is retrieval-only ([enrichment] enabled=false) — skipping structural atlas + Tier-2 RAPTOR"
                        );
                        return;
                    }
                    use corpus_engine::enrichment::state::{EnrichmentPhase, EnrichmentStateFile};
                    use sovereign_tools::atlas_postinstall::{
                        build_structural_atlas, build_triage_candidates, effective_tier2_budget,
                        StructuralAtlasOutcome, TriageOutcome,
                    };
                    tracing::info!(corpus = %cid, "post-install: structural atlas — start");
                    // Generic enrichment state stamp so every corpus's
                    // post-install gets a row in
                    // `_enrichment_state.json` and the desktop chip
                    // can render "Extracting atoms" → "Saving" →
                    // "complete". Daemon restart leaves a Stalled
                    // entry for the sweeper to pick up.
                    let corpus_index_dir = indexes.join(&cid);
                    let _ = EnrichmentStateFile::stamp(
                        &corpus_index_dir,
                        &cid,
                        Some("structural_atlas"),
                        EnrichmentPhase::AtomExtraction,
                        0,
                        0,
                        Some("walking chunks for structural atom extraction"),
                    );
                    let atlas_ok = match build_structural_atlas(&cid, indexes.clone(), recipes)
                        .await
                    {
                        StructuralAtlasOutcome::Built {
                            atoms_path,
                            edges_path,
                            elapsed_secs,
                        } => {
                            tracing::info!(
                                corpus = %cid,
                                atoms = %atoms_path.display(),
                                edges = %edges_path.display(),
                                elapsed_s = elapsed_secs,
                                "post-install: structural atlas — built"
                            );
                            let _ = EnrichmentStateFile::stamp(
                                &corpus_index_dir,
                                &cid,
                                Some("structural_atlas"),
                                EnrichmentPhase::Complete,
                                0,
                                0,
                                Some(&format!("structural atlas built in {elapsed_secs}s")),
                            );
                            // Glassbox: enrichment is heavy local
                            // inference work — record it so the
                            // Activity surface shows "enriched <corpus>"
                            // distinct from the raw ingest embed pass.
                            enrich_activity.record(ActivityEventKind::CorpusEnriched {
                                corpus_id: cid.clone(),
                                atoms: 0,
                                duration_secs: elapsed_secs as u64,
                            });
                            true
                        }
                        StructuralAtlasOutcome::AlreadyPresent { atoms_path } => {
                            tracing::info!(
                                corpus = %cid,
                                atoms = %atoms_path.display(),
                                "post-install: structural atlas — already present"
                            );
                            let _ = EnrichmentStateFile::stamp(
                                &corpus_index_dir,
                                &cid,
                                Some("structural_atlas"),
                                EnrichmentPhase::Complete,
                                0,
                                0,
                                Some("structural atlas already present"),
                            );
                            true
                        }
                        StructuralAtlasOutcome::Failed { reason } => {
                            tracing::warn!(
                                corpus = %cid,
                                reason,
                                "post-install: structural atlas — failed (atlas grounding stays off until rebuilt)"
                            );
                            let _ = EnrichmentStateFile::fail(
                                &corpus_index_dir,
                                &cid,
                                &format!("structural atlas: {reason}"),
                            );
                            false
                        }
                    };

                    // Triage: rank in-corpus articles by centrality
                    // and persist the top-N for Tier-2 enrichment.
                    // Output is consumable by `sovereign enrich init
                    // --include-articles <path>` so the manual flow
                    // and the future daemon-side scheduler share one
                    // source of truth.
                    if atlas_ok {
                        // Honour per-corpus override (Phase B3) —
                        // operators set this via `sovereign atlas
                        // budget <corpus> <n>`. Default is 1000
                        // articles, which fits L1+L2+L3 with tier
                        // headroom on a wiki-scale atlas.
                        let budget = effective_tier2_budget(&indexes, &cid);
                        tracing::info!(
                            corpus = %cid,
                            budget,
                            "post-install: triage — start"
                        );
                        let triage_path_for_tier2 =
                            match build_triage_candidates(&cid, indexes.clone(), budget).await {
                                TriageOutcome::Built {
                                    path,
                                    in_corpus_picked,
                                    elapsed_secs,
                                } => {
                                    tracing::info!(
                                        corpus = %cid,
                                        path = %path.display(),
                                        articles = in_corpus_picked,
                                        elapsed_s = elapsed_secs,
                                        "post-install: triage — built"
                                    );
                                    Some(path)
                                }
                                TriageOutcome::NoAtlas => {
                                    tracing::warn!(
                                        corpus = %cid,
                                        "post-install: triage skipped (atlas missing)"
                                    );
                                    None
                                }
                                TriageOutcome::Failed { reason } => {
                                    tracing::warn!(
                                        corpus = %cid,
                                        reason,
                                        "post-install: triage failed"
                                    );
                                    None
                                }
                            };

                        // Tier-2 extraction: kick off the long-running
                        // background job that runs Phase 1 over every
                        // chapter of every triaged article. Detached
                        // subprocess — daemon doesn't block on it,
                        // logs go to <workspace>/extraction.log, and
                        // restart safety comes from the per-chapter
                        // checkpoint inherited from `enrich extract
                        // --resume`.
                        if let Some(triage_path) = triage_path_for_tier2 {
                            use sovereign_tools::atlas_postinstall::{
                                launch_tier2_extraction_with_advice, Tier2LaunchOutcome,
                            };
                            // Deep Tier-2 needs a real `sovereign-cli-llm`.
                            // If none is configured, skip the pass rather
                            // than self-execing the daemon/GUI binary (see
                            // `resolve_enrich_cli`). Nothing runs after this
                            // block in the spawned task, so an early return
                            // just ends the (already-detached) task cleanly;
                            // the structural atlas + inline tiered enrichment
                            // stamped above stay intact — we do NOT mark the
                            // corpus failed for skipping an optional pass.
                            let Some(cli_bin) = resolve_enrich_cli() else {
                                tracing::info!(
                                    corpus = %cid,
                                    "post-install: tier-2 deep extraction skipped — no `sovereign-cli-llm` configured (set $SOVEREIGN_CLI_LLM_BIN to run the referential-atlas pass); structural atlas + inline tiered enrichment already complete"
                                );
                                return;
                            };
                            let enrich_dir = indexes
                                .parent()
                                .unwrap_or(std::path::Path::new("."))
                                .join("enrichment");

                            // Phase C3: walk the live mesh and ask
                            // whether any peer already has a deeper
                            // atlas. If so, skip local extraction
                            // and log the recommendation — operator
                            // pulls via the canonical-sync surface.
                            let peer_advice =
                                gather_peer_atlas_advice(&state_for_task, &cid, &indexes).await;
                            if let Some(advice) = peer_advice.as_ref() {
                                tracing::info!(
                                    corpus = %cid,
                                    peer = %advice.peer_name,
                                    peer_tier2 = advice.peer_tier2_count,
                                    local_tier2 = advice.local_tier2_count,
                                    "post-install: tier-2 extraction — deferring to peer (Phase C3)"
                                );
                            } else {
                                tracing::info!(
                                    corpus = %cid,
                                    "post-install: tier-2 extraction — launching background"
                                );
                            }
                            match launch_tier2_extraction_with_advice(
                                &cid,
                                triage_path,
                                cli_bin,
                                enrich_dir,
                                indexes.clone(),
                                peer_advice,
                            )
                            .await
                            {
                                Tier2LaunchOutcome::Spawned {
                                    workspace_id,
                                    log_path,
                                    pid,
                                } => tracing::info!(
                                    corpus = %cid,
                                    workspace = %workspace_id,
                                    log = %log_path.display(),
                                    pid,
                                    "post-install: tier-2 extraction — spawned (tail extraction.log for progress)"
                                ),
                                Tier2LaunchOutcome::AlreadyComplete {
                                    workspace_id,
                                    chapters_done,
                                    chapters_total,
                                } => tracing::info!(
                                    corpus = %cid,
                                    workspace = %workspace_id,
                                    chapters_done,
                                    chapters_total,
                                    "post-install: tier-2 extraction — already complete"
                                ),
                                Tier2LaunchOutcome::DeferredToPeer {
                                    peer_name,
                                    peer_tier2_count,
                                    local_tier2_count,
                                } => tracing::info!(
                                    corpus = %cid,
                                    peer = %peer_name,
                                    peer_tier2 = peer_tier2_count,
                                    local_tier2 = local_tier2_count,
                                    "post-install: tier-2 extraction — deferred to peer (run `sovereign mesh canonical-pull {cid} --from {peer_name}` to fetch)"
                                ),
                                Tier2LaunchOutcome::InitFailed { reason }
                                | Tier2LaunchOutcome::SpawnFailed { reason } => tracing::warn!(
                                    corpus = %cid,
                                    reason,
                                    "post-install: tier-2 extraction — launch failed"
                                ),
                            }
                        }
                    }
                });
            }
            Err(corpus_engine::Error::Cancelled(_)) => {
                // Cancel route handles the wipe; we only clean up
                // the progress map so the UI returns to
                // "not_installed" on the next poll.
                state_for_task
                    .inner
                    .corpus_progress
                    .write()
                    .await
                    .remove(&corpus_id_for_task);
                tracing::info!(
                    corpus = %corpus_id_for_task,
                    "spawn_corpus_install: ingest cancelled"
                );
            }
            Err(e) => {
                // Recorded, not merely logged — see `record_failure`.
                // A log-only handler here is the bug that made every
                // ingest failure render as a completed install.
                record_failure(&state_for_task, &corpus_id_for_task, e.to_string()).await;
                tracing::warn!(
                    corpus = %corpus_id_for_task,
                    error = %e,
                    "spawn_corpus_install: ingest failed"
                );
            }
        }
    });
    InstallOutcome::Spawned
}

/// Bool projection of [`spawn_corpus_install_outcome`]: `true` only when
/// a new task was spawned, `false` for every no-op or failure. Kept for
/// the mesh auto-ingest / OICP callers that only need "did we start
/// work?" and don't map failures to HTTP status codes. The HTTP install
/// handler uses the outcome variant directly so it can surface 4xx.
pub async fn spawn_corpus_install_with_parameters(
    state: AppState,
    corpus_id: String,
    parameters: std::collections::BTreeMap<String, serde_json::Value>,
) -> bool {
    matches!(
        spawn_corpus_install_outcome(state, corpus_id, parameters).await,
        InstallOutcome::Spawned
    )
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub corpus_id: String,
    /// Recipe-parameter values supplied by the user at install time.
    /// Validated against the recipe's `[recipe.parameters]` schema
    /// before the ingest task spawns, so a missing required param
    /// fails the request rather than silently producing an empty
    /// corpus. JSON shape: `{"name": value, ...}` where value can
    /// be a string, integer, or string array.
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Convert a JSON parameter map (the API's wire format) into a TOML
/// parameter map, which is what
/// [`Recipe::resolve_parameters`](corpus_engine::Recipe::resolve_parameters)
/// expects. JSON strings → TOML strings, JSON integers → TOML ints,
/// JSON arrays of strings → TOML arrays. Anything else fails with a
/// helpful error.
fn json_params_to_toml(
    params: &std::collections::BTreeMap<String, serde_json::Value>,
) -> std::result::Result<std::collections::BTreeMap<String, toml::Value>, String> {
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in params {
        let toml_value = match v {
            serde_json::Value::String(s) => toml::Value::String(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    toml::Value::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    toml::Value::Float(f)
                } else {
                    return Err(format!("parameter `{k}` is a non-finite number"));
                }
            }
            serde_json::Value::Bool(b) => toml::Value::Boolean(*b),
            serde_json::Value::Array(arr) => {
                let mut items = Vec::with_capacity(arr.len());
                for item in arr {
                    match item {
                        serde_json::Value::String(s) => items.push(toml::Value::String(s.clone())),
                        other => {
                            return Err(format!(
                                "parameter `{k}` array entries must be strings, \
                                 got: {other:?}"
                            ))
                        }
                    }
                }
                toml::Value::Array(items)
            }
            serde_json::Value::Null => continue,
            serde_json::Value::Object(_) => {
                return Err(format!(
                    "parameter `{k}` is a JSON object — only string, int, \
                     bool, and string array values are supported"
                ));
            }
        };
        out.insert(k.clone(), toml_value);
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct InstallResponse {
    pub corpus_id: String,
    /// True when a new task was spawned, false when an ingest for
    /// this corpus was already running on this node.
    pub spawned: bool,
}

/// The outcome of an install attempt, richer than the `bool` the
/// mesh/OICP callers consume. It exists so the HTTP handler can tell a
/// *benign* idempotent no-op (`AlreadyActive`) apart from a *genuine
/// failure* (`RecipeNotFound` / `InvalidParameters`) — the former is a
/// 200 with `spawned:false`, the latter a 4xx with a reason. Before this
/// split, every non-spawn collapsed to `spawned:false` + HTTP 200, so a
/// mistyped corpus id or a private recipe the daemon can't resolve looked
/// identical to "already running" and the CLI printed "Install requested"
/// over a silent failure. Glassbox: the caller now sees why nothing ran.
pub enum InstallOutcome {
    /// A new background ingest task was started.
    Spawned,
    /// An ingest for this corpus was already in flight — no new task.
    AlreadyActive,
    /// No corpus engine is wired on this node.
    NoEngine,
    /// The recipe could not be resolved: no local override, no catalog
    /// entry, and no bundled fallback. Carries the resolver's message.
    RecipeNotFound(String),
    /// Supplied parameters failed JSON→TOML coercion or schema
    /// validation against the recipe's `[recipe.parameters]` block.
    InvalidParameters(String),
}

#[derive(Debug, Serialize)]
pub struct ProgressSnapshotResponse {
    pub progress: std::collections::HashMap<String, corpus_engine::IngestProgress>,
}

/// Signal the corpus's cancellation flag and wait (bounded) for the
/// in-flight ingest task to exit. Shared between `/pause` and `/cancel`
/// — both want a clean stop before they decide what to do with on-disk
/// state.
///
/// Returns whether a live task was actually signalled. After this
/// helper returns the corpus is no longer in `active_ingests` (or the
/// 5 s ceiling was hit and we've logged a warning).
async fn stop_in_flight_ingest(
    state: &AppState,
    engine: &corpus_engine::CorpusEngine,
    corpus_id: &str,
) -> bool {
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
            let still_active = state.inner.active_ingests.read().await.contains(corpus_id);
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
    state.inner.corpus_progress.write().await.remove(corpus_id);

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
    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        )
    })?;

    let cancelled = stop_in_flight_ingest(&state, engine, &req.corpus_id).await;

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

    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        )
    })?;

    let cancelled = stop_in_flight_ingest(&state, engine, &req.corpus_id).await;

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
