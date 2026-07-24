// SPDX-License-Identifier: AGPL-3.0-or-later
//! Document asset HTTP routes.
//!
//! Exposes the document library and per-document conversation API
//! under `/v1/documents/*`. All routes go through the standard auth
//! middleware — they're merged into the `authed` router in `main.rs`.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;

use sovereign_core::runtime::Runtime;
use sovereign_core::types::*;
use sovereign_tools::document_asset::{DocumentAssetManager, IngestProgress};

use crate::auth::TenantId;

// ─── Request/Response types ──────────────────────────────────

#[derive(serde::Deserialize)]
pub struct UploadRequest {
    /// Absolute path to the file on the server's filesystem.
    /// For the desktop app this is the local file path.
    pub file_path: String,
}

#[derive(serde::Serialize)]
pub struct AssetResponse {
    pub asset: DocumentAsset,
}

#[derive(serde::Deserialize)]
pub struct AskRequest {
    pub question: String,
}

#[derive(serde::Serialize)]
pub struct AskResponse {
    pub response: String,
    pub operation: DocumentAssetOperation,
    pub sources: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct AssetStateResponse {
    pub state: AssetState,
}

#[derive(serde::Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;

fn api_error(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
}

// ─── Router ──────────────────────────────────────────────────

pub fn document_router() -> Router {
    Router::new()
        .route("/v1/documents", get(list_assets))
        .route("/v1/documents/upload", post(upload_asset))
        .route("/v1/documents/{id}", get(get_asset).delete(delete_asset))
        .route("/v1/documents/{id}/ask", post(ask_asset))
        .route("/v1/documents/{id}/state", get(get_asset_state))
}

// ─── Helpers ─────────────────────────────────────────────────

/// Build a DocumentAssetManager from the Runtime's shared resources.
/// This is cheap — it just clones Arcs.
///
/// When the Runtime carries a GLiNER entity extractor (installed for
/// retrieval), reuse it for the ingest skeleton's T2 entity pass — the
/// same −70%-token NER-for-LLM swap the desktop uses. Absent/not-warm
/// falls back to the LLM per window, so this is safe unconditionally.
fn manager_from_runtime(runtime: &Runtime) -> DocumentAssetManager {
    let manager =
        DocumentAssetManager::new(Arc::clone(&runtime.inference), Arc::clone(&runtime.store));
    match &runtime.gliner {
        Some(extractor) => manager.with_entity_extractor(Arc::clone(extractor)),
        None => manager,
    }
}

// ─── Handlers ────────────────────────────────────────────────

/// A document is visible to `tenant` unless it is owned by a DIFFERENT
/// principal. Unowned (legacy / single-user) and own documents are visible —
/// the same deny-set rule the corpus surfaces use (`forbidden_corpora`).
fn asset_visible_to(asset: &DocumentAsset, tenant: &str) -> bool {
    match &asset.owner {
        Some(owner) => owner == tenant,
        None => true,
    }
}

/// GET /v1/documents
async fn list_assets(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
) -> ApiResult<Vec<DocumentAsset>> {
    let mut assets = runtime
        .store
        .list_document_assets()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    assets.retain(|a| asset_visible_to(a, &tenant.0));
    Ok(Json(assets))
}

/// POST /v1/documents/upload
async fn upload_asset(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Json(body): Json<UploadRequest>,
) -> ApiResult<AssetResponse> {
    let path = std::path::Path::new(&body.file_path);
    if !path.exists() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            &format!("File not found: {}", body.file_path),
        ));
    }

    let manager = manager_from_runtime(&runtime);

    let mut asset = manager
        .ingest(path, |progress| {
            // Log progress server-side. In the desktop app, Tauri events
            // push these to the frontend; the HTTP API is polled via
            // GET /v1/documents/{id}/state instead.
            match &progress {
                IngestProgress::Indexing { done, total } => {
                    tracing::debug!("Indexing {done}/{total}");
                }
                IngestProgress::BuildingSkeleton { done, total } => {
                    tracing::debug!("Skeleton {done}/{total}");
                }
                IngestProgress::Ready { asset_id, .. } => {
                    tracing::info!("Asset {asset_id} ready");
                }
                _ => {}
            }
        })
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    // Stamp ownership so this document is private to the uploading tenant on
    // a multi-user hub (re-saves the asset row with `owner` set).
    asset.owner = Some(tenant.0.clone());
    runtime
        .store
        .save_document_asset(&asset)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    Ok(Json(AssetResponse { asset }))
}

/// GET /v1/documents/:id
async fn get_asset(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Path(id): Path<String>,
) -> ApiResult<AssetResponse> {
    let asset = runtime
        .store
        .get_document_asset(&id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Document not found"))?;

    if !asset_visible_to(&asset, &tenant.0) {
        return Err(api_error(StatusCode::NOT_FOUND, "Document not found"));
    }

    Ok(Json(AssetResponse { asset }))
}

/// DELETE /v1/documents/:id
async fn delete_asset(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    // Only the owner may delete. Treat another principal's document as
    // not-found rather than revealing that it exists.
    let asset = runtime
        .store
        .get_document_asset(&id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Document not found"))?;
    if !asset_visible_to(&asset, &tenant.0) {
        return Err(api_error(StatusCode::NOT_FOUND, "Document not found"));
    }

    let manager = manager_from_runtime(&runtime);
    manager
        .delete(&id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    Ok(Json(serde_json::json!({"deleted": true})))
}

/// POST /v1/documents/:id/ask
async fn ask_asset(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Path(id): Path<String>,
    Json(body): Json<AskRequest>,
) -> ApiResult<AskResponse> {
    let asset = runtime
        .store
        .get_document_asset(&id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Document not found"))?;

    if !asset_visible_to(&asset, &tenant.0) {
        return Err(api_error(StatusCode::NOT_FOUND, "Document not found"));
    }

    if !asset.state.is_queryable() {
        return Err(api_error(
            StatusCode::CONFLICT,
            &format!(
                "Document is not ready for queries (state: {})",
                asset.state.label()
            ),
        ));
    }

    let manager = manager_from_runtime(&runtime);
    let start = std::time::Instant::now();

    let (response, operation, sources) = manager
        .ask(&asset, &body.question, |_progress| {
            // HTTP clients poll state — no push needed.
        })
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    let duration_ms = start.elapsed().as_millis() as u64;

    // Record the operation for analytics.
    let message_id = uuid::Uuid::new_v4().to_string();
    let _ = runtime
        .store
        .save_document_operation(&message_id, &asset.id, &operation, duration_ms)
        .await;

    Ok(Json(AskResponse {
        response,
        operation,
        sources,
    }))
}

/// GET /v1/documents/:id/state
async fn get_asset_state(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Path(id): Path<String>,
) -> ApiResult<AssetStateResponse> {
    let asset = runtime
        .store
        .get_document_asset(&id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Document not found"))?;

    if !asset_visible_to(&asset, &tenant.0) {
        return Err(api_error(StatusCode::NOT_FOUND, "Document not found"));
    }

    Ok(Json(AssetStateResponse { state: asset.state }))
}
