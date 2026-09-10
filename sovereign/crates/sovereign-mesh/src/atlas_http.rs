// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas-browse HTTP — `/internal/atlas/...` (sv-surface D4).
//!
//! The wire form of the desktop's Atlas Inspector. Until this router
//! the twelve `atlas_*` Tauri commands each opened the atlas
//! themselves: six over `AppState.corpus_engine` through
//! `sovereign_tools::atlas_view::FileAtlasReader`, six over a private
//! `Arc<SqliteStateStore>`. In attach mode that meant the desktop
//! carried a second corpus engine purely to browse an atlas the
//! daemon already had open — the §10.6 twin the sv-surface campaign
//! exists to delete.
//!
//! **The computation did not move; the process did.** Every handler
//! here builds the SAME `FileAtlasReader` over the daemon's own
//! `engine.index_dir()` and calls the SAME method the Tauri command
//! called, returning the SAME type. That is what makes the desktop
//! rung a repoint rather than a rewrite: the response bodies
//! deserialise into `sovereign_tools::atlas_view::*` unchanged.
//!
//! # What is here, and what is not
//!
//! Served (6 routes for the 6 `corpus_engine` commands):
//!
//! | Desktop command | Route |
//! |---|---|
//! | `atlas_list_corpora` | `GET  /internal/atlas/corpora` |
//! | `atlas_build_report` | `GET  /internal/atlas/{corpus}/report` |
//! | `atlas_list_members` | `GET  /internal/atlas/{corpus}/members` |
//! | `atlas_list_atoms` | `POST /internal/atlas/{corpus}/atoms` |
//! | `atlas_subgraph` | `GET  /internal/atlas/{corpus}/subgraph` |
//! | `atlas_get_atom_detail` | `GET  /internal/atlas/{corpus}/atoms/{atom_id}` |
//!
//! `list_atoms` is a POST because its input is a structured
//! `{filter, page}` — `AtomFilter` carries a `Vec<String>` of
//! subtypes, which no flat query string expresses without a second
//! encoding. Same house style as `POST /v1/knowledge/search`: a read,
//! asked with a body.
//!
//! NOT served, and named rather than quietly dropped (ARCH §18.3):
//! the six conversation-tiered commands
//! (`atlas_list_conv_corpora`, `atlas_list_conversations`,
//! `atlas_get_conv_detail`, `atlas_get_entity_aggregate`,
//! `atlas_get_chunk_entity_progress`, `atlas_get_conv_entities`).
//! Their seven store calls —
//! `list_conv_corpora_with_state_buckets`,
//! `list_conversations_paginated`, `get_conv_skeleton`,
//! `get_active_correction`, `aggregate_entity`,
//! `list_conv_raptor_nodes`, `get_chunk_entity_progress` — are
//! INHERENT methods on the concrete `sovereign_store::sqlite::
//! SqliteStateStore`. `ServingCore` holds an `Arc<dyn StateStore>`,
//! which is a supertrait of twelve sub-traits and names none of
//! them, and `sovereign_core::conv_tiered::ConvTieredReader` (which
//! the `Runtime` does carry) covers only two of the seven. Serving
//! them therefore requires widening a trait in `sovereign-core` and
//! `sovereign-contracts` and updating every implementor — a change
//! outside this router, tracked as the remainder of D4.
//!
//! Also app-local by decision, not by omission: the two GLiNER model
//! commands (`atlas_check_gliner_model`, `atlas_download_gliner_model`)
//! manage a file the desktop downloads for its own extractor.
//!
//! Loopback posture is `reading_http`'s, unchanged: router-level
//! [`crate::loopback_guard::loopback_only`] middleware plus a
//! per-handler `enforce_localhost`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_tools::atlas_view::{
    AtlasViewError, AtomFilter, AtomQueryError, FileAtlasReader, PageCursor, DEFAULT_MAX_NODES,
};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

// ─── Request shapes ────────────────────────────────────────────

/// Body of `POST /internal/atlas/{corpus}/atoms` — the two arguments
/// `atlas_list_atoms` takes, with the same `Option` semantics
/// (`None` = the type's `Default`).
#[derive(Debug, Default, Deserialize)]
pub struct AtomBrowseRequest {
    #[serde(default)]
    pub filter: Option<AtomFilter>,
    #[serde(default)]
    pub page: Option<PageCursor>,
}

#[derive(Debug, Deserialize)]
pub struct SubgraphQuery {
    /// Node cap. Absent = `atlas_view::DEFAULT_MAX_NODES`, which is
    /// exactly what `atlas_subgraph` passes when the desktop omits it
    /// — one decider for the cap, still in `sovereign-tools`.
    #[serde(default)]
    pub max_nodes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The atlas-browse router. Mounted unconditionally on every serving
/// daemon's client router beside `reading_http`; a daemon with no
/// corpus engine answers 503 with that named reason rather than 404,
/// so "not built" and "not mounted" stay different facts.
pub fn atlas_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/atlas/corpora", get(list_corpora))
        .route("/internal/atlas/{corpus}/report", get(build_report))
        .route("/internal/atlas/{corpus}/members", get(list_members))
        .route("/internal/atlas/{corpus}/atoms", post(list_atoms))
        .route("/internal/atlas/{corpus}/subgraph", get(subgraph))
        .route("/internal/atlas/{corpus}/atoms/{atom_id}", get(atom_detail))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET /internal/atlas/corpora — every installed corpus that has an
/// atlas, with per-atom-type counts. Wire form of
/// `atlas_list_corpora`; answers `Vec<AtlasCorpusSummary>`.
async fn list_corpora(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let reader = match reader_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match reader.list_corpora().await {
        Ok(rows) => {
            tracing::debug!(corpora = rows.len(), "atlas_http: corpora listed");
            (StatusCode::OK, Json(rows)).into_response()
        }
        Err(e) => view_error(&e),
    }
}

/// GET /internal/atlas/{corpus}/report — what the last build found.
/// Wire form of `atlas_build_report`; answers `AtlasBuildReport`.
///
/// A corpus whose report step never ran comes back `reported: false`
/// — a successful answer, not an error. That distinction is the
/// reader's, kept here unchanged.
async fn build_report(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let reader = match reader_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match reader.build_report(&corpus).await {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(e) => view_error(&e),
    }
}

/// GET /internal/atlas/{corpus}/members — the member atlases of a
/// collection corpus. Wire form of `atlas_list_members`; answers
/// `Vec<AtlasMemberSummary>`.
///
/// An EMPTY list is the correct answer for every ordinary corpus, and
/// the frontend branches on it to pick which Explore surface to
/// render — so this must never become a 404.
async fn list_members(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let reader = match reader_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match reader.list_members(&corpus).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => view_error(&e),
    }
}

/// POST /internal/atlas/{corpus}/atoms — filterable, paginated atom
/// browse. Wire form of `atlas_list_atoms`; answers `AtomListPage`.
async fn list_atoms(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    body: Option<Json<AtomBrowseRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let reader = match reader_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let filter = req.filter.unwrap_or_default();
    let page = req.page.unwrap_or_default();
    match reader.list_atoms(&corpus, filter, page).await {
        Ok(page) => {
            tracing::debug!(
                corpus = %corpus,
                returned = page.items.len(),
                total_matching = page.total_matching,
                next_offset = ?page.next_offset,
                "atlas_http: atom page served",
            );
            (StatusCode::OK, Json(page)).into_response()
        }
        Err(e) => atom_error(&e),
    }
}

/// GET /internal/atlas/{corpus}/subgraph?max_nodes= — the curated
/// landscape map. Wire form of `atlas_subgraph`; answers
/// `AtlasSubgraph`.
async fn subgraph(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(SubgraphQuery { max_nodes }): Query<SubgraphQuery>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let reader = match reader_for(&daemon) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match reader
        .subgraph(&corpus, max_nodes.unwrap_or(DEFAULT_MAX_NODES))
        .await
    {
        Ok(graph) => (StatusCode::OK, Json(graph)).into_response(),
        Err(e) => atom_error(&e),
    }
}

/// GET /internal/atlas/{corpus}/atoms/{atom_id} — the full inspector
/// record. Wire form of `atlas_get_atom_detail`; answers `AtomDetail`,
/// or 404 when the atom is not in this corpus's atoms.json (a stale UI
/// link, or extraction renumbered ids since the last browse).
///
/// Carries the desktop command's second half too: evidence excerpts
/// carry a `section_id`, and the reading surface needs a numeric
/// `chunk_id` to deep-link to. Building that map is a full
/// `chunks.lance` scan (2.8 GB / ~90 s on Wikipedia), so it is NEVER
/// built on the click path — resolved from the per-corpus cache when
/// ready, and otherwise left `None` (the row renders non-clickable)
/// while a ONE-TIME background build fills the cache for later
/// clicks. Same policy, same cache key, now one copy for every
/// surface instead of one per surface.
async fn atom_detail(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, atom_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let engine = match daemon.corpus_engine() {
        Some(e) => Arc::clone(e),
        None => return service_unavailable("corpus engine not initialised"),
    };
    let reader = FileAtlasReader::new(engine.index_dir().to_path_buf());
    let mut detail = match reader.get_atom_detail(&corpus, &atom_id).await {
        Ok(Some(d)) => d,
        Ok(None) => return not_found("atom not found"),
        Err(e) => return atom_error(&e),
    };

    let needs_sections = detail
        .evidence_excerpts
        .iter()
        .any(|e| !e.section_id.is_empty());
    if needs_sections {
        if let Some(map) = section_map::resolve_or_build(&engine, &corpus) {
            for excerpt in &mut detail.evidence_excerpts {
                excerpt.chunk_id = map.get(&excerpt.section_id).copied();
            }
        }
    }
    (StatusCode::OK, Json(detail)).into_response()
}

// ─── section_id → chunk_id cache ───────────────────────────────

mod section_map {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use corpus_engine::CorpusEngine;

    /// Per-corpus state. `Building` is a marker, not a value: it stops
    /// a second click from launching a second full-index scan while
    /// the first is still running.
    enum State {
        Building,
        Ready(Arc<HashMap<String, u64>>),
    }

    fn cache() -> &'static Mutex<HashMap<String, State>> {
        static CACHE: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Take the cache lock, tolerating poison.
    ///
    /// The desktop's copy of this cache called `.unwrap()` here, which
    /// makes ONE panicking handler wedge atom-detail resolution for the
    /// life of the process. Nothing under this lock can leave the map
    /// in a state a later reader misreads — the values are whole maps,
    /// inserted or removed atomically — so recovering the guard is
    /// strictly better than propagating a panic the caller cannot act
    /// on.
    fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<String, State>> {
        cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The cached map when it is ready; otherwise `None`, having kicked
    /// off a one-time background build. Never blocks the caller on the
    /// scan — that is the whole policy.
    pub(super) fn resolve_or_build(
        engine: &Arc<CorpusEngine>,
        corpus_id: &str,
    ) -> Option<Arc<HashMap<String, u64>>> {
        let mut cache = lock_cache();
        if let Some(State::Ready(map)) = cache.get(corpus_id) {
            return Some(Arc::clone(map));
        }
        if cache.contains_key(corpus_id) {
            // `Building` — a scan is already running for this corpus.
            return None;
        }
        cache.insert(corpus_id.to_string(), State::Building);
        drop(cache);

        let engine = Arc::clone(engine);
        let corpus = corpus_id.to_string();
        tokio::spawn(async move {
            let built = match engine.open_index_for_corpus(&corpus).await {
                Ok(index) => index.section_chunk_index().await.ok(),
                Err(_) => None,
            };
            let mut cache = lock_cache();
            match built {
                Some(map) => {
                    tracing::info!(
                        corpus_id = %corpus,
                        sections = map.len(),
                        "atlas_http: section→chunk map built + cached (background)",
                    );
                    cache.insert(corpus, State::Ready(Arc::new(map)));
                }
                None => {
                    // Drop the marker so a later click retries instead of
                    // wedging on `Building` forever.
                    cache.remove(&corpus);
                }
            }
        });
        None
    }
}

// ─── Helpers ───────────────────────────────────────────────────

/// The reader over the DAEMON's indexes dir — one construction site,
/// so no handler can accidentally point at a different root.
fn reader_for(daemon: &Arc<EmbeddedDaemon>) -> Result<FileAtlasReader, axum::response::Response> {
    match daemon.corpus_engine() {
        Some(engine) => Ok(FileAtlasReader::new(engine.index_dir().to_path_buf())),
        None => Err(service_unavailable("corpus engine not initialised")),
    }
}

/// `AtlasViewError` → status. `CorpusNotFound` is the caller's
/// mistake (404); an unreadable indexes dir is ours (500).
fn view_error(e: &AtlasViewError) -> axum::response::Response {
    match e {
        AtlasViewError::CorpusNotFound(_) => not_found(&e.to_string()),
        AtlasViewError::IndexesDir(_) => internal_error(&e.to_string()),
    }
}

/// `AtomQueryError` → status, on the same rule.
fn atom_error(e: &AtomQueryError) -> axum::response::Response {
    match e {
        AtomQueryError::UnknownCorpus(_) => not_found(&e.to_string()),
        AtomQueryError::ReadAtoms(_) | AtomQueryError::Task(_) => internal_error(&e.to_string()),
    }
}

fn not_found(msg: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

fn internal_error(msg: &str) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

fn service_unavailable(msg: &str) -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}
