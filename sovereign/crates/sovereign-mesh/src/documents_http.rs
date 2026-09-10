// SPDX-License-Identifier: AGPL-3.0-or-later
//! The document-asset family — `/v1/documents` (sv-surface D9a).
//!
//! # Why a family, and why now
//!
//! `commands/document_asset.rs` carries twelve needle reads, every one
//! live on an attached boot because the desktop spine builds a store
//! and an inference handle in BOTH modes. Six of its eight commands
//! are pure CRUD over the daemon's own `StateStore` — the asset
//! records, the legacy `documents` table, the chunks behind them — and
//! read the DESKTOP's store handle. On a real attached boot that is
//! the same sqlite file only because setup adopted `data_dir`
//! (invariant `attach_mode_data_dir_split`); it is not a fact the
//! surface may rely on, and the campaign's rule is that a store read
//! is a daemon fact.
//!
//! | Route | Object | Desktop command it retires |
//! |---|---|---|
//! | `GET /v1/documents` | `StateStore::list_document_assets` | `list_document_assets` |
//! | `GET /v1/documents/legacy` | `list_sources` + `get_chunks_by_source` | `list_legacy_documents` |
//! | `POST /v1/documents/legacy/promote` | `save_document_asset` | `promote_legacy_document` |
//! | `GET /v1/documents/{id}` | `StateStore::get_document_asset` | `get_document_asset` |
//! | `DELETE /v1/documents/{id}` | `DocumentAssetManager::delete` | `delete_document_asset` |
//! | `POST /v1/documents/{id}/skeleton` | `DocumentAssetManager::rebuild_skeleton` | `rebuild_document_skeleton` |
//!
//! `DocumentAsset` crosses WHOLE — it is already
//! `Serialize + Deserialize` in `sovereign-contracts` and the desktop
//! already returns it verbatim, so there is no projection to keep in
//! step (ARCH §2). Only the legacy row and the promotion rule are
//! minted here, and both are minted here because this is now their one
//! implementation.
//!
//! # The two deciders that moved DOWN, not across
//!
//! **The legacy listing** is not a store call — it is a fold with
//! three rules (skip a source an asset already owns, skip a
//! `corpus:` source, skip a source with no chunks) plus a word count
//! and a filename derivation. The desktop had the only copy. So did
//! **the promotion**, which mints a `DocumentAsset` from stored chunks
//! with a title rule (`strip the extension, underscores and hyphens
//! become spaces`) that nothing else in the workspace can reproduce.
//! Serving them from here makes each one decider (§10.6); leaving the
//! fold above the wire and serving only its inputs would have kept the
//! twin and added a round trip per source.
//!
//! # What does NOT cross, named rather than half-served
//!
//! **`upload_document_asset`.** The ingest itself is not the obstacle
//! — the daemon is on loopback, so the path it is handed is a path it
//! can open. The obstacle is that `run_ingest` narrates: the banner,
//! the percentage and the ETA are driven by a `document:progress`
//! event per embed batch, and a request/response route can deliver
//! exactly one frame. A route that started the ingest and returned
//! would replace a live banner with a spinner, which is feature loss,
//! and the campaign's second directive forbids it. It wants the frame
//! stream `turn_http`'s socket already is — `lc_http`'s
//! `/{corpus_id}/ingest/progress` is the polling shape that answers
//! the same question for local corpora, and the document family should
//! take that shape or the socket, deliberately, not by accident here.
//!
//! **`ask_document`'s document-operation half.** `route` +
//! `execute_operation` is a Fast-slot classification followed by a
//! generation: a TURN in everything but name. Its off-topic and
//! empty-RAG halves already ride the wire (f7fe8cfff) through the ONE
//! driver. Serving the document-op path from a CRUD route here would
//! mint a second turn driver beside `serve_turn`, which is the bar
//! TOPOLOGY Phase 6 exists to hold. It belongs on the driver, as a
//! document-attached turn — not on this file's surface.
//!
//! # The entity extractor, named
//!
//! `POST /{id}/skeleton` builds its manager WITHOUT an
//! `EntityExtractor`, because a daemon holds none: the desktop reads
//! `state.entity_extractor` and the serving commission has no
//! equivalent field. `build_skeleton` documents the fallback (the LLM
//! per-window entity pass) and takes it, so the answer is the same
//! shape at a higher token cost, not a degraded one. Wiring the
//! extractor onto `ServingCore` is a real rung; claiming it is here
//! would be worse than saying so.
//!
//! Loopback posture is `reading_http`'s, unchanged: router-level
//! middleware plus a per-handler `enforce_localhost` (ARCH §5, defence
//! in depth).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::DocumentAsset;
use sovereign_tools::document_asset::DocumentAssetManager;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

// ─── The wire projections ──────────────────────────────────────

/// `GET /v1/documents`'s body.
///
/// An object rather than a bare array for the reason every list route
/// in this crate gives: a top-level array cannot grow a sibling field
/// (a count, a truncation marker) without breaking every caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentListResponse {
    pub documents: Vec<DocumentAsset>,
}

/// `GET /v1/documents/{id}` and the two write routes' body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentResponse {
    pub document: DocumentAsset,
}

/// A document in the legacy `documents` table with no `DocumentAsset`
/// record — an upload from the old paperclip path.
///
/// The desktop's `LegacyDocumentEntry`, moved: it is `Serialize`-only
/// up there (a Tauri return), and a wire type has to parse back, which
/// is why this carries `Deserialize` too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyDocumentEntry {
    /// The chunk store's `source` key — the promotion handle.
    pub source: String,
    /// Last path segment of `source`.
    pub filename: String,
    pub chunk_count: usize,
    pub word_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyDocumentListResponse {
    pub documents: Vec<LegacyDocumentEntry>,
}

/// `POST /v1/documents/legacy/promote`'s body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromoteLegacyRequest {
    /// The `source` key from a [`LegacyDocumentEntry`].
    pub source: String,
}

/// `DELETE /v1/documents/{id}`'s body.
///
/// A body rather than a 204, so a caller that only reads bodies still
/// learns that the delete happened (§18.3) — and so the route has
/// somewhere to say what it removed if it ever reports more.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletedResponse {
    pub deleted: bool,
    pub id: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The document-asset router. Mounted unconditionally on serving
/// daemons; a commission with no `StateStore` answers a named 503,
/// which is a different fact from an unmounted route's 404 (§18.3).
///
/// `/v1/documents/legacy` is registered BEFORE `/v1/documents/{id}` —
/// axum 0.8 prefers the static segment either way, and the order here
/// says so to the reader rather than relying on it silently.
pub fn documents_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/documents", get(list_documents))
        .route("/v1/documents/legacy", get(list_legacy_documents))
        .route("/v1/documents/legacy/promote", post(promote_legacy))
        .route(
            "/v1/documents/{id}",
            get(get_document).delete(delete_document),
        )
        .route("/v1/documents/{id}/skeleton", post(rebuild_skeleton))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/documents` — every ingested document asset.
///
/// Store order, which is the order the desktop's picker already
/// rendered. Sorting here would be a presentation decision the caller
/// owns.
async fn list_documents(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match store.list_document_assets().await {
        Ok(documents) => {
            tracing::debug!(count = documents.len(), "documents_http: assets listed");
            Json(DocumentListResponse { documents }).into_response()
        }
        Err(e) => internal_error("list_document_assets", &e.to_string()),
    }
}

/// GET `/v1/documents/{id}` — one asset record.
///
/// 404 for an id the store does not hold. The command it replaces
/// answered `Ok(None)`, which the frontend rendered as "not ready yet"
/// — the same shape as an asset mid-ingest. Two different facts, and
/// the pane offers a different remedy for each (§18.3).
async fn get_document(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match store.get_document_asset(&id).await {
        Ok(Some(document)) => {
            tracing::debug!(asset = %id, state = %document.state.label(), "documents_http: asset read");
            Json(DocumentResponse { document }).into_response()
        }
        Ok(None) => not_found(&id),
        Err(e) => internal_error("get_document_asset", &e.to_string()),
    }
}

/// DELETE `/v1/documents/{id}` — remove an asset and its chunks.
///
/// Through `DocumentAssetManager::delete`, which owns the removal
/// order (chunks first, then the record). A raw
/// `StateStore::delete_document_asset` here would be a second, shorter
/// implementation of the same delete and would leave the chunks behind
/// (§10.6).
async fn delete_document(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let manager = match manager_for(&daemon) {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    match manager.delete(&id).await {
        Ok(()) => {
            tracing::debug!(asset = %id, "documents_http: asset deleted");
            Json(DeletedResponse { deleted: true, id }).into_response()
        }
        Err(e) => internal_error("delete", &e.to_string()),
    }
}

/// POST `/v1/documents/{id}/skeleton` — rebuild the structural
/// skeleton from stored chunks and answer the refreshed record.
///
/// The self-heal the desktop spawns when `ask_document` finds
/// `skeleton: None`, and the button the picker offers, are the same
/// operation — this is it. Works from chunks, so no file is required
/// and an asset whose original ingest was interrupted can be repaired
/// from history.
///
/// The refreshed asset comes from a re-READ, not from the rebuild's
/// return: `rebuild_skeleton` answers a `DocumentSkeleton`, and the
/// caller needs the record whose `state` and `document_type` the
/// rebuild also moved.
async fn rebuild_skeleton(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let manager = match manager_for(&daemon) {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let skeleton = match manager.rebuild_skeleton(&id).await {
        Ok(s) => s,
        Err(e) => return internal_error("rebuild_skeleton", &e.to_string()),
    };
    match store.get_document_asset(&id).await {
        Ok(Some(document)) => {
            tracing::debug!(
                asset = %id,
                entities = skeleton.main_entities.len(),
                sections = skeleton.sections.len(),
                "documents_http: skeleton rebuilt"
            );
            Json(DocumentResponse { document }).into_response()
        }
        // The rebuild succeeded and the record is gone: a concurrent
        // delete. Reported, not papered over with the pre-rebuild copy.
        Ok(None) => internal_error(
            "rebuild_skeleton",
            &format!("asset '{id}' vanished between the rebuild and the read-back"),
        ),
        Err(e) => internal_error("get_document_asset", &e.to_string()),
    }
}

/// GET `/v1/documents/legacy` — documents in the old `documents` table
/// that no `DocumentAsset` owns.
///
/// The fold, moved down whole (see the header). Three skips and a word
/// count; a `corpus:` source is corpus content, not an upload, and an
/// `asset:` source an asset already claims is that asset's own chunks.
async fn list_legacy_documents(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let sources = match store.list_sources().await {
        Ok(s) => s,
        Err(e) => return internal_error("list_sources", &e.to_string()),
    };
    // An asset listing that FAILS is not "no assets": every
    // asset-owned source would then be reported as a promotable
    // legacy document, and promoting one would mint a duplicate
    // record. It is an error (§18.3) — the desktop's
    // `unwrap_or_default()` here was the swallow.
    let assets = match store.list_document_assets().await {
        Ok(a) => a,
        Err(e) => return internal_error("list_document_assets", &e.to_string()),
    };
    let asset_sources: std::collections::HashSet<String> =
        assets.iter().map(|a| format!("asset:{}", a.id)).collect();

    let mut documents = Vec::new();
    for source in &sources {
        if source.starts_with("asset:") && asset_sources.contains(source) {
            continue;
        }
        if source.starts_with("corpus:") {
            continue;
        }
        let chunks = match store.get_chunks_by_source(source).await {
            Ok(c) => c,
            Err(e) => return internal_error("get_chunks_by_source", &e.to_string()),
        };
        if chunks.is_empty() {
            continue;
        }
        documents.push(LegacyDocumentEntry {
            source: source.clone(),
            filename: filename_of(source),
            chunk_count: chunks.len(),
            word_count: word_count_of(&chunks),
        });
    }
    tracing::debug!(
        sources = sources.len(),
        legacy = documents.len(),
        "documents_http: legacy documents listed"
    );
    Json(LegacyDocumentListResponse { documents }).into_response()
}

/// POST `/v1/documents/legacy/promote` — mint a `DocumentAsset` over
/// chunks that are already in the store.
///
/// No re-upload and no re-embedding: the chunks exist, this gives them
/// a record. `skeleton` stays `None` and `state` is `PartiallyReady`,
/// which is honest — the structural pass has not run, and
/// `POST /{id}/skeleton` is what runs it.
async fn promote_legacy(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<PromoteLegacyRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let chunks = match store.get_chunks_by_source(&req.source).await {
        Ok(c) => c,
        Err(e) => return internal_error("get_chunks_by_source", &e.to_string()),
    };
    if chunks.is_empty() {
        return error_body(
            StatusCode::NOT_FOUND,
            &format!("no chunks are stored for source '{}'", req.source),
        );
    }
    let filename = filename_of(&req.source);
    let document = DocumentAsset {
        id: uuid::Uuid::new_v4().to_string(),
        title: title_of(&filename),
        filename,
        // Unknown for a legacy document: the original file is gone and
        // the chunks do not record its size. `0.0` is the store's own
        // "not recorded", carried over from the command verbatim.
        file_size_mb: 0.0,
        word_count: word_count_of(&chunks),
        chunk_count: chunks.len(),
        document_type: sovereign_core::types::DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("legacy:{}", req.source),
        skeleton: None,
        state: sovereign_core::types::AssetState::PartiallyReady,
        owner: None,
    };
    if let Err(e) = store.save_document_asset(&document).await {
        return internal_error("save_document_asset", &e.to_string());
    }
    tracing::debug!(
        source = %req.source,
        asset = %document.id,
        chunks = document.chunk_count,
        "documents_http: legacy document promoted"
    );
    Json(DocumentResponse { document }).into_response()
}

// ─── Helpers ───────────────────────────────────────────────────

/// Last path segment of a chunk `source`. One implementation, called
/// by the listing and by the promotion, so the row a caller picks and
/// the record it promotes cannot carry different names (§10.6).
fn filename_of(source: &str) -> String {
    source.rsplit('/').next().unwrap_or(source).to_string()
}

/// The display title derived from a filename: extension stripped,
/// underscores and hyphens read as spaces.
fn title_of(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(name, _)| name)
        .unwrap_or(filename)
        .replace(['_', '-'], " ")
}

fn word_count_of(chunks: &[sovereign_core::types::DocumentChunk]) -> usize {
    chunks
        .iter()
        .map(|c| c.content.split_whitespace().count())
        .sum()
}

/// The daemon's own `StateStore`. One lookup site, so no handler can
/// read a different store than the one a turn writes to.
fn store_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<dyn StateStore>, Response> {
    daemon.state_store().map(Arc::clone).ok_or_else(|| {
        error_body(
            StatusCode::SERVICE_UNAVAILABLE,
            "this daemon holds no StateStore (it was commissioned to serve nothing)",
        )
    })
}

/// A `DocumentAssetManager` over the daemon's store and the provider
/// the serving `Runtime` answers turns with.
///
/// `runtime.inference` rather than `ServingCore.inference_provider`
/// deliberately: a document operation is inference the user attributes
/// to "the model I am talking to", and reading the runtime's handle
/// makes that structurally the same object (§10.6). It also means the
/// 503 here names the `Runtime`, which is the object whose absence
/// actually stops the operation.
fn manager_for(daemon: &Arc<EmbeddedDaemon>) -> Result<DocumentAssetManager, Response> {
    let store = store_for(daemon)?;
    let runtime = daemon.runtime().ok_or_else(|| {
        error_body(
            StatusCode::SERVICE_UNAVAILABLE,
            "this daemon serves no turns (it was commissioned without a Runtime), so it \
             holds no inference provider to run a document operation with",
        )
    })?;
    let inference: Arc<dyn InferenceProvider> = Arc::clone(&runtime.inference);
    Ok(DocumentAssetManager::new(inference, store))
}

fn not_found(id: &str) -> Response {
    error_body(
        StatusCode::NOT_FOUND,
        &format!("no document asset '{id}' is stored on this daemon"),
    )
}

fn internal_error(op: &str, detail: &str) -> Response {
    tracing::warn!(
        operation = op,
        detail,
        "documents_http: store operation failed"
    );
    error_body(
        StatusCode::INTERNAL_SERVER_ERROR,
        &format!("{op}: {detail}"),
    )
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

/// Keeps the two DTOs this file mints named in its own surface, the
/// way `lc_http` does: a reader looking for "what shape comes back"
/// finds it without leaving the module.
#[allow(dead_code)]
fn _answers(e: LegacyDocumentEntry, d: DocumentAsset) -> (LegacyDocumentEntry, DocumentAsset) {
    (e, d)
}
