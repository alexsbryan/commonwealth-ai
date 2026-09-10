// SPDX-License-Identifier: AGPL-3.0-or-later
//! MeshApp explorer HTTP — `/internal/meshapp/...` (sv-surface D3, the
//! thirteen `resolve_index_path` reads).
//!
//! The wire form of the desktop's MeshApp explorer. `atlas_http` +
//! `reading_http`'s atom page took the three ATOM readers
//! (`meshapp_read_corpus`, `meshapp_search_parcels`,
//! `meshapp_parcel_analytics`); these are the OTHER thirteen, every one
//! of which resolved an installed corpus's on-disk index in the desktop
//! process (`commands/meshapp.rs::resolve_index_path`, :321) and called
//! a `sovereign_meshapp::*` projection over it.
//!
//! **The computation did not move; the process did.** Each handler
//! resolves the DAEMON's index dir through the same
//! `CorpusEngine::installed_indexes()` lookup the desktop ran, and calls
//! the SAME `sovereign_meshapp` function with the SAME arguments,
//! returning the SAME DTO. The desktop rung is a repoint.
//!
//! | Desktop command (`commands/meshapp.rs`) | Route | `sovereign_meshapp` op |
//! |---|---|---|
//! | `meshapp_graph` :346 | `GET  {c}/graph` | `load_graph` + `graph_nodes` |
//! | `meshapp_node` :367 | `GET  {c}/nodes/{id}` | `load_graph` + `node_detail` |
//! | `meshapp_findings` :382 | `GET  {c}/findings` | `load_graph` + `findings` |
//! | `meshapp_search_entities` :398 | `GET  {c}/entities` | `load_graph` + `search_entities` |
//! | `meshapp_claims` :425 | `GET  {c}/claims` | `load_claims` |
//! | `meshapp_questions` :441 | `GET  {c}/questions` | `load_questions` |
//! | `meshapp_reconciliation` :457 | `GET  {c}/reconciliation` | `reconciliation` |
//! | `meshapp_subgraph` :471 | `GET  {c}/subgraph` | `load_graph` + `subgraph` |
//! | `meshapp_corpus_stats` :492 | `GET  {c}/stats` | `corpus_stats` |
//! | `meshapp_timeline` :506 | `GET  {c}/timeline` | `timeline` |
//! | `meshapp_read_chunk` :522 | `GET  {c}/chunks/{chunk_id}` | `read_chunk` |
//! | `meshapp_document_feed` :546 | `GET  {c}/documents` | `document_feed` |
//! | `meshapp_wrapped_artifact` :568 | `GET  {c}/wrapped` | `wrapped::wrapped_artifact` |
//!
//! (`{c}` is `/internal/meshapp/{corpus}`.) Four of the thirteen share
//! one input — the investigation/atlas graph — so they are grouped in
//! the source below under one `graph_for`, which is where `load_graph`
//! is called exactly once per request.
//!
//! # What does NOT cross, by decision
//!
//! The per-command `authorize(&installs, webview.label(), MeshStoreRead)`
//! gate stays desktop-side (campaign row X2). The token is the webview
//! LABEL, host-assigned at window creation to a window in THAT process;
//! it is not a bearer credential and there is nothing to send. Modelling
//! authorization on the wire would mean inventing a second, weaker
//! token — so these routes carry `reading_http`'s posture instead
//! (router-level [`crate::loopback_guard::loopback_only`] plus a
//! per-handler `enforce_localhost`) and the grant check runs before the
//! desktop ever calls one.
//!
//! # Where the limits are decided
//!
//! The clamps below (`50/500` graph nodes, `25/100` entity hits,
//! `100/500` claims and questions, `30/80` subgraph nodes, `14` docs in
//! `1..=90`) are the desktop command's own numbers, moved here. That
//! makes the route the ONE decider (ARCH §10.6): the repoint drops the
//! desktop's copy rather than keeping a second clamp that could drift
//! from this one. An over-large request is served CLAMPED, never
//! refused — same as `reading_http`'s atom page.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_meshapp::{Graph, MeshAppError};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

// ─── The clamps (one decider, ARCH §10.6) ──────────────────────

/// `meshapp_graph` / `meshapp_findings`-adjacent node listing.
const GRAPH_LIMIT_DEFAULT: usize = 50;
const GRAPH_LIMIT_MAX: usize = 500;
/// `meshapp_search_entities`.
const ENTITY_LIMIT_DEFAULT: usize = 25;
const ENTITY_LIMIT_MAX: usize = 100;
/// `meshapp_claims` / `meshapp_questions`.
const ATOM_LIMIT_DEFAULT: usize = 100;
const ATOM_LIMIT_MAX: usize = 500;
/// `meshapp_subgraph`.
const SUBGRAPH_LIMIT_DEFAULT: usize = 30;
const SUBGRAPH_LIMIT_MAX: usize = 80;
/// `meshapp_document_feed`.
const FEED_DOCS_DEFAULT: usize = 14;
const FEED_DOCS_MIN: usize = 1;
const FEED_DOCS_MAX: usize = 90;

// ─── Query shapes ──────────────────────────────────────────────

/// `?node_type=&limit=` — `meshapp_graph` and `meshapp_subgraph`'s two
/// arguments. `None` means the command's default, applied below.
#[derive(Debug, Default, Deserialize)]
pub struct NodeListQuery {
    #[serde(default)]
    pub node_type: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `?pattern=` — `meshapp_findings`'s optional substring filter.
#[derive(Debug, Default, Deserialize)]
pub struct FindingsQuery {
    #[serde(default)]
    pub pattern: Option<String>,
}

/// `?q=&node_type=&limit=` — `meshapp_search_entities`.
#[derive(Debug, Default, Deserialize)]
pub struct EntitySearchQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub node_type: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `?limit=` — `meshapp_claims` / `meshapp_questions`.
#[derive(Debug, Default, Deserialize)]
pub struct AtomLimitQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `?limit_docs=` — `meshapp_document_feed`.
#[derive(Debug, Default, Deserialize)]
pub struct FeedQuery {
    #[serde(default)]
    pub limit_docs: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The MeshApp explorer router. Mounted unconditionally on every
/// serving daemon's client router beside `reading_http` and
/// `atlas_http`; a daemon with no corpus engine answers 503 with that
/// named reason, which is a different fact from an unmounted router's
/// 404.
pub fn meshapp_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/internal/meshapp/{corpus}/graph", get(graph))
        .route("/internal/meshapp/{corpus}/nodes/{id}", get(node))
        .route("/internal/meshapp/{corpus}/findings", get(findings))
        .route("/internal/meshapp/{corpus}/entities", get(search_entities))
        .route("/internal/meshapp/{corpus}/claims", get(claims))
        .route("/internal/meshapp/{corpus}/questions", get(questions))
        .route(
            "/internal/meshapp/{corpus}/reconciliation",
            get(reconciliation),
        )
        .route("/internal/meshapp/{corpus}/subgraph", get(subgraph))
        .route("/internal/meshapp/{corpus}/stats", get(corpus_stats))
        .route("/internal/meshapp/{corpus}/timeline", get(timeline))
        .route("/internal/meshapp/{corpus}/chunks/{chunk_id}", get(chunk))
        .route("/internal/meshapp/{corpus}/documents", get(document_feed))
        .route("/internal/meshapp/{corpus}/wrapped", get(wrapped))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

// ─── The four graph projections ────────────────────────────────
//
// One input — the investigation graph, or the atlas adapted into one —
// and four folds over it. `graph_for` is where `load_graph` is called,
// once per request, so no handler can pick a different root or a
// different adapter.

/// GET `/internal/meshapp/{corpus}/graph?node_type=&limit=` — the wire
/// form of `meshapp_graph`. Degree-ranked entities, highest first;
/// answers `Vec<sovereign_meshapp::GraphNodeDto>`.
async fn graph(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<NodeListQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let g = match graph_for(&daemon, &corpus).await {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let limit = clamp(q.limit, GRAPH_LIMIT_DEFAULT, GRAPH_LIMIT_MAX);
    let rows = sovereign_meshapp::graph_nodes(&g, q.node_type.as_deref(), limit);
    tracing::debug!(
        corpus = %corpus,
        node_type = q.node_type.as_deref().unwrap_or("*"),
        limit,
        returned = rows.len(),
        "meshapp_http: graph nodes served",
    );
    (StatusCode::OK, Json(rows)).into_response()
}

/// GET `/internal/meshapp/{corpus}/nodes/{id}` — the wire form of
/// `meshapp_node`. One entity plus every incident edge; answers
/// `sovereign_meshapp::NodeDetailDto`, or 404 when the id is not in the
/// graph (a stale link, or the graph was rebuilt).
async fn node(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let g = match graph_for(&daemon, &corpus).await {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    match sovereign_meshapp::node_detail(&g, &id) {
        Ok(detail) => (StatusCode::OK, Json(detail)).into_response(),
        Err(e) => absent_or_internal(&corpus, &e),
    }
}

/// GET `/internal/meshapp/{corpus}/findings?pattern=` — the wire form
/// of `meshapp_findings`; answers `Vec<sovereign_meshapp::FindingDto>`.
///
/// An empty list is the right answer for a corpus whose graph carries
/// no findings, so this never 404s on emptiness.
async fn findings(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<FindingsQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let g = match graph_for(&daemon, &corpus).await {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let rows = sovereign_meshapp::findings(&g, q.pattern.as_deref());
    (StatusCode::OK, Json(rows)).into_response()
}

/// GET `/internal/meshapp/{corpus}/entities?q=&node_type=&limit=` — the
/// wire form of `meshapp_search_entities`; answers
/// `Vec<sovereign_meshapp::GraphNodeDto>`.
///
/// An empty or whitespace `q` answers `[]` WITHOUT opening the graph —
/// the command's own short-circuit, kept, because a blank search box
/// must not cost a full graph load on every keystroke.
async fn search_entities(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(params): Query<EntitySearchQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let query = params.q.unwrap_or_default();
    if query.trim().is_empty() {
        return (
            StatusCode::OK,
            Json(Vec::<sovereign_meshapp::GraphNodeDto>::new()),
        )
            .into_response();
    }
    let g = match graph_for(&daemon, &corpus).await {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let limit = clamp(params.limit, ENTITY_LIMIT_DEFAULT, ENTITY_LIMIT_MAX);
    let rows = sovereign_meshapp::search_entities(&g, &query, params.node_type.as_deref(), limit);
    tracing::debug!(
        corpus = %corpus,
        limit,
        returned = rows.len(),
        "meshapp_http: entity search served",
    );
    (StatusCode::OK, Json(rows)).into_response()
}

/// GET `/internal/meshapp/{corpus}/subgraph?node_type=&limit=` — the
/// wire form of `meshapp_subgraph`; answers
/// `sovereign_meshapp::SubgraphDto`.
///
/// Distinct from `atlas_http`'s `/internal/atlas/{corpus}/subgraph`,
/// which is `atlas_view`'s curated landscape map over a different
/// projection. Two surfaces, two shapes, two paths — naming them apart
/// is what stops one being served where the other was meant.
async fn subgraph(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<NodeListQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let g = match graph_for(&daemon, &corpus).await {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let limit = clamp(q.limit, SUBGRAPH_LIMIT_DEFAULT, SUBGRAPH_LIMIT_MAX);
    let dto = sovereign_meshapp::subgraph(&g, q.node_type.as_deref(), limit);
    (StatusCode::OK, Json(dto)).into_response()
}

// ─── The nine index-path projections ───────────────────────────

/// GET `/internal/meshapp/{corpus}/claims?limit=` — the wire form of
/// `meshapp_claims`; answers `Vec<sovereign_meshapp::ClaimDto>`.
async fn claims(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<AtomLimitQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let limit = clamp(q.limit, ATOM_LIMIT_DEFAULT, ATOM_LIMIT_MAX);
    match sovereign_meshapp::load_claims(&path, limit) {
        Ok(rows) => {
            tracing::debug!(corpus = %corpus, limit, returned = rows.len(),
                "meshapp_http: claims served");
            (StatusCode::OK, Json(rows)).into_response()
        }
        Err(e) => absent_or_internal(&corpus, &e),
    }
}

/// GET `/internal/meshapp/{corpus}/questions?limit=` — the wire form of
/// `meshapp_questions`; answers `Vec<sovereign_meshapp::QuestionDto>`.
async fn questions(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<AtomLimitQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let limit = clamp(q.limit, ATOM_LIMIT_DEFAULT, ATOM_LIMIT_MAX);
    match sovereign_meshapp::load_questions(&path, limit) {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => absent_or_internal(&corpus, &e),
    }
}

/// GET `/internal/meshapp/{corpus}/reconciliation` — the wire form of
/// `meshapp_reconciliation`; answers
/// `Vec<sovereign_meshapp::ReconciliationMergeDto>`.
///
/// The library op is infallible (a corpus with no merge log yields an
/// empty vec), so this handler has no error arm of its own — the only
/// failure it can report is "that corpus is not installed here".
async fn reconciliation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let rows = sovereign_meshapp::reconciliation(&path);
    (StatusCode::OK, Json(rows)).into_response()
}

/// GET `/internal/meshapp/{corpus}/stats` — the wire form of
/// `meshapp_corpus_stats`; answers
/// `sovereign_meshapp::CorpusStatsDto`. Infallible, like
/// `reconciliation` above.
async fn corpus_stats(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let dto = sovereign_meshapp::corpus_stats(&path);
    (StatusCode::OK, Json(dto)).into_response()
}

/// GET `/internal/meshapp/{corpus}/timeline` — the wire form of
/// `meshapp_timeline`; answers `sovereign_meshapp::TimelineDto`.
async fn timeline(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match sovereign_meshapp::timeline(&path).await {
        Ok(dto) => (StatusCode::OK, Json(dto)).into_response(),
        Err(e) => absent_or_internal(&corpus, &e),
    }
}

/// GET `/internal/meshapp/{corpus}/chunks/{chunk_id}` — the wire form
/// of `meshapp_read_chunk`; answers `sovereign_meshapp::ChunkDto`.
///
/// `chunk_id` is the NUMERIC id an edge carries. A non-numeric segment
/// is a 400 with that reason — the command's own parse, kept here so
/// the caller learns it asked wrongly rather than that the chunk is
/// missing. A numeric id that is not in the index is a 404.
async fn chunk(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, chunk_id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Ok(id) = chunk_id.trim().parse::<u64>() else {
        return bad_request(&format!("chunk id `{chunk_id}` is not a numeric id"));
    };
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match sovereign_meshapp::read_chunk(&path, id).await {
        Ok(dto) => (StatusCode::OK, Json(dto)).into_response(),
        Err(e) => absent_or_internal(&corpus, &e),
    }
}

/// GET `/internal/meshapp/{corpus}/documents?limit_docs=` — the wire
/// form of `meshapp_document_feed`; answers
/// `sovereign_meshapp::DocumentFeedDto`.
async fn document_feed(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
    Query(q): Query<FeedQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let limit_docs = q
        .limit_docs
        .unwrap_or(FEED_DOCS_DEFAULT)
        .clamp(FEED_DOCS_MIN, FEED_DOCS_MAX);
    match sovereign_meshapp::document_feed(&path, limit_docs).await {
        Ok(dto) => {
            tracing::debug!(corpus = %corpus, limit_docs, "meshapp_http: document feed served");
            (StatusCode::OK, Json(dto)).into_response()
        }
        Err(e) => absent_or_internal(&corpus, &e),
    }
}

/// GET `/internal/meshapp/{corpus}/wrapped` — the wire form of
/// `meshapp_wrapped_artifact`; answers
/// `sovereign_meshapp::wrapped::WrappedArtifact`.
///
/// The library op serves the cached `wrapped/all-time.json` when fresh
/// and rebuilds otherwise — a pure Rust fold, no inference. Its second
/// argument is the state db whose GLiNER `chunk_entities` rows fill the
/// entity cards; the desktop passed `svrnmesh_root()/sovereign.db`, and
/// the daemon passes ITS OWN data root's, which is the same file
/// whenever the daemon owns that root (the one-writer rule
/// `ServingCore::state_store` documents). When the file is absent those
/// cards are simply absent from the deck, which is the library's
/// behaviour and not a substitution made here.
async fn wrapped(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(corpus): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let path = match index_path(&daemon, &corpus).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let state_db = daemon.data_dir().join("sovereign.db");
    let state_db = state_db.exists().then_some(state_db);
    match sovereign_meshapp::wrapped::wrapped_artifact(&path, state_db.as_deref()).await {
        Ok(artifact) => {
            tracing::info!(
                corpus = %corpus,
                state_db = state_db.is_some(),
                "meshapp_http: wrapped artifact served",
            );
            (StatusCode::OK, Json(artifact)).into_response()
        }
        // `wrapped::wrapped_artifact` still answers a `String`, and it
        // is the one read here with NO absence case: every way it fails
        // (unopenable index, unreadable `_corpus_meta.json`, a rejected
        // verbatim audit) is a failure, never "you asked for something
        // that is not there". So it is a 500 unconditionally — which is
        // exactly what the phrase table decided for it.
        Err(e) => {
            tracing::warn!(corpus = %corpus, error = %e, "meshapp_http: wrapped failed");
            internal_error(&format!("`{corpus}`: {e}"))
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────

/// Resolve an installed corpus's on-disk index directory over the
/// DAEMON's engine — the wire twin of `commands/meshapp.rs`'s
/// `resolve_index_path` (:321), same lookup, same two failures named
/// apart: no engine at all is a 503, an uninstalled corpus is a 404.
///
/// One construction site, so no handler can point at a different root.
async fn index_path(daemon: &Arc<EmbeddedDaemon>, corpus_id: &str) -> Result<PathBuf, Response> {
    let Some(engine) = daemon.corpus_engine() else {
        return Err(service_unavailable("corpus engine not initialised"));
    };
    let installed = engine
        .installed_indexes()
        .await
        .map_err(|e| internal_error(&format!("installed_indexes: {e}")))?;
    installed
        .iter()
        .find(|i| i.corpus_id == corpus_id)
        .map(|i| i.path.clone())
        .ok_or_else(|| not_found(&format!("corpus `{corpus_id}` is not installed")))
}

/// The investigation graph (or the atlas adapted into one) for a
/// corpus. The four graph handlers all come through here, so
/// `load_graph` has exactly one call site in this router.
async fn graph_for(daemon: &Arc<EmbeddedDaemon>, corpus_id: &str) -> Result<Graph, Response> {
    let path = index_path(daemon, corpus_id).await?;
    sovereign_meshapp::load_graph(&path).map_err(|e| absent_or_internal(corpus_id, &e))
}

/// `limit` → the applied value. An absent limit takes the default; an
/// over-large one is CLAMPED and served, not refused.
fn clamp(requested: Option<usize>, default: usize, max: usize) -> usize {
    requested.unwrap_or(default).min(max)
}

/// A `sovereign-meshapp` failure → a status. The library answers
/// [`MeshAppError`] since 2026-09-10, so an ABSENCE — an unknown entity
/// id, an unknown chunk id, a corpus with no graph to explore — is
/// separated from a failed read BY TYPE, at the site that knows.
/// Until then this read a table of error phrases; the table is gone
/// (ARCH §2.1, §18.3).
fn absent_or_internal(corpus_id: &str, err: &MeshAppError) -> Response {
    let msg = format!("`{corpus_id}`: {err}");
    if err.is_absence() {
        tracing::debug!(corpus = %corpus_id, error = %err, "meshapp_http: absent");
        not_found(&msg)
    } else {
        tracing::warn!(corpus = %corpus_id, error = %err, "meshapp_http: read failed");
        internal_error(&msg)
    }
}

fn error_body(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

fn bad_request(msg: &str) -> Response {
    error_body(StatusCode::BAD_REQUEST, msg)
}

fn not_found(msg: &str) -> Response {
    error_body(StatusCode::NOT_FOUND, msg)
}

fn internal_error(msg: &str) -> Response {
    error_body(StatusCode::INTERNAL_SERVER_ERROR, msg)
}

fn service_unavailable(msg: &str) -> Response {
    error_body(StatusCode::SERVICE_UNAVAILABLE, msg)
}
