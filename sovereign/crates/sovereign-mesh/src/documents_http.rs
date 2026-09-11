// SPDX-License-Identifier: AGPL-3.0-or-later
//! The document-asset family — `/v1/documents` (sv-surface D9a).
//!
//! Six routes over the daemon's own `StateStore` and `DocumentAssetManager`:
//! list, the legacy `documents` listing, promote, get, delete, rebuild
//! skeleton. `commands/document_asset.rs` read the DESKTOP's store handle,
//! the same sqlite file only because setup adopted `data_dir` (invariant
//! `attach_mode_data_dir_split`). `DocumentAsset` crosses WHOLE; only the
//! legacy fold and the promotion rule are minted here, because this is now
//! their one implementation (§10.6).
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! Does NOT cross, named rather than half-served:
//! - **`upload_document_asset`** — `run_ingest` narrates one frame per embed
//!   batch and a request/response route delivers exactly one.
//! - **`ask_document`'s document-operation half** — a route plus a generation
//!   is a TURN; serving it here mints a second driver beside `serve_turn`.
//!
//! Named degradation: `POST /{id}/skeleton` builds its manager with no
//! `EntityExtractor` because a daemon holds none, so `build_skeleton` takes
//! its documented LLM fallback — same shape, higher token cost.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::DocumentAsset;
use sovereign_tools::document_asset::DocumentAssetManager;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{json_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

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
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/documents` — every ingested document asset.
///
/// Store order, which is the order the desktop's picker already
/// rendered. Sorting here would be a presentation decision the caller
/// owns.
async fn list_documents(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.list_document_assets().await {
        Ok(documents) => {
            tracing::debug!(count = documents.len(), "documents_http: assets listed");
            Json(DocumentListResponse { documents }).into_response()
        }
        Err(e) => internal_error("list_document_assets", &e.to_string()),
    })
}

/// GET `/v1/documents/{id}` — one asset record.
///
/// 404 for an id the store does not hold. The command it replaces
/// answered `Ok(None)`, which the frontend rendered as "not ready yet"
/// — the same shape as an asset mid-ingest. Two different facts, and
/// the pane offers a different remedy for each (§18.3).
async fn get_document(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.get_document_asset(&id).await {
        Ok(Some(document)) => {
            tracing::debug!(asset = %id, state = %document.state.label(), "documents_http: asset read");
            Json(DocumentResponse { document }).into_response()
        }
        Ok(None) => not_found(&id),
        Err(e) => internal_error("get_document_asset", &e.to_string()),
    })
}

/// DELETE `/v1/documents/{id}` — remove an asset and its chunks.
///
/// Through `DocumentAssetManager::delete`, which owns the removal
/// order (chunks first, then the record). A raw
/// `StateStore::delete_document_asset` here would be a second, shorter
/// implementation of the same delete and would leave the chunks behind
/// (§10.6).
async fn delete_document(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let manager = manager_for(&daemon)?;
    Ok(match manager.delete(&id).await {
        Ok(()) => {
            tracing::debug!(asset = %id, "documents_http: asset deleted");
            Json(DeletedResponse { deleted: true, id }).into_response()
        }
        Err(e) => internal_error("delete", &e.to_string()),
    })
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let manager = manager_for(&daemon)?;
    let skeleton = match manager.rebuild_skeleton(&id).await {
        Ok(s) => s,
        Err(e) => return Ok(internal_error("rebuild_skeleton", &e.to_string())),
    };
    Ok(match store.get_document_asset(&id).await {
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
    })
}

/// GET `/v1/documents/legacy` — documents in the old `documents` table
/// that no `DocumentAsset` owns.
///
/// The fold, moved down whole (see the header). Three skips and a word
/// count; a `corpus:` source is corpus content, not an upload, and an
/// `asset:` source an asset already claims is that asset's own chunks.
async fn list_legacy_documents(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let sources = match store.list_sources().await {
        Ok(s) => s,
        Err(e) => return Ok(internal_error("list_sources", &e.to_string())),
    };
    // An asset listing that FAILS is not "no assets": every
    // asset-owned source would then be reported as a promotable
    // legacy document, and promoting one would mint a duplicate
    // record. It is an error (§18.3) — the desktop's
    // `unwrap_or_default()` here was the swallow.
    let assets = match store.list_document_assets().await {
        Ok(a) => a,
        Err(e) => return Ok(internal_error("list_document_assets", &e.to_string())),
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
            Err(e) => return Ok(internal_error("get_chunks_by_source", &e.to_string())),
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
    Ok(Json(LegacyDocumentListResponse { documents }).into_response())
}

/// POST `/v1/documents/legacy/promote` — mint a `DocumentAsset` over
/// chunks that are already in the store.
///
/// No re-upload and no re-embedding: the chunks exist, this gives them
/// a record. `skeleton` stays `None` and `state` is `PartiallyReady`,
/// which is honest — the structural pass has not run, and
/// `POST /{id}/skeleton` is what runs it.
async fn promote_legacy(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<PromoteLegacyRequest>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    let chunks = match store.get_chunks_by_source(&req.source).await {
        Ok(c) => c,
        Err(e) => return Ok(internal_error("get_chunks_by_source", &e.to_string())),
    };
    if chunks.is_empty() {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            &format!("no chunks are stored for source '{}'", req.source),
        ));
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
        return Ok(internal_error("save_document_asset", &e.to_string()));
    }
    tracing::debug!(
        source = %req.source,
        asset = %document.id,
        chunks = document.chunk_count,
        "documents_http: legacy document promoted"
    );
    Ok(Json(DocumentResponse { document }).into_response())
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
fn store_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<dyn StateStore>, Absence> {
    daemon.state_store().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable(
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
fn manager_for(daemon: &Arc<EmbeddedDaemon>) -> Result<DocumentAssetManager, Absence> {
    let store = store_for(daemon)?;
    let runtime = daemon.runtime().ok_or_else(|| {
        Absence::unavailable(
            "this daemon serves no turns (it was commissioned without a Runtime), so it \
             holds no inference provider to run a document operation with",
        )
    })?;
    let inference: Arc<dyn InferenceProvider> = Arc::clone(&runtime.inference);
    Ok(DocumentAssetManager::new(inference, store))
}

fn not_found(id: &str) -> Response {
    json_error(
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
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &format!("{op}: {detail}"),
    )
}

/// Keeps the two DTOs this file mints named in its own surface, the
/// way `lc_http` does: a reader looking for "what shape comes back"
/// finds it without leaving the module.
#[allow(dead_code)]
fn _answers(e: LegacyDocumentEntry, d: DocumentAsset) -> (LegacyDocumentEntry, DocumentAsset) {
    (e, d)
}
