//! Client-facing routes for mesh app management and proxying.
//!
//! GET    /v1/apps                   — list registered apps
//! POST   /v1/apps/{app_id}/install  — install an app from its manifest
//! GET    /v1/apps/{app_id}/status   — app status on this node
//! DELETE /v1/apps/{app_id}          — uninstall an app
//! ANY    /app/{app_id}/*path        — proxy to the running app

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use commonwealth_app::manifest::MeshAppManifest;

use crate::state::AppState;

/// Response body for `GET /v1/apps`.
#[derive(Serialize)]
pub struct AppListResponse {
    pub apps: Vec<MeshAppManifest>,
}

/// Request body for `POST /v1/apps/{app_id}/install`.
#[derive(Deserialize)]
pub struct InstallRequest {
    /// The full manifest to register.
    pub manifest: MeshAppManifest,
}

/// Response for app status.
#[derive(Serialize)]
pub struct AppStatusResponse {
    pub app_id: String,
    pub registered: bool,
    pub running: bool,
    pub port: Option<u16>,
}

/// `GET /v1/apps` — list all known apps.
pub async fn list_apps(State(state): State<AppState>) -> impl IntoResponse {
    let apps = state.inner.app_registry.list().await;
    Json(AppListResponse { apps })
}

/// `POST /v1/apps/{app_id}/install` — register an app manifest.
pub async fn install_app(
    State(state): State<AppState>,
    Path(app_id): Path<String>,
    Json(body): Json<InstallRequest>,
) -> impl IntoResponse {
    if body.manifest.app_id != app_id {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "app_id in path does not match manifest"})),
        )
            .into_response();
    }
    state.inner.app_registry.register(body.manifest).await;
    (StatusCode::OK, Json(serde_json::json!({"status": "registered"}))).into_response()
}

/// `GET /v1/apps/{app_id}/status` — check if an app is registered and running locally.
pub async fn app_status(
    State(state): State<AppState>,
    Path(app_id): Path<String>,
) -> impl IntoResponse {
    let registered = state.inner.app_registry.get(&app_id).await.is_some();
    let port = state.inner.app_port_map.get(&app_id).await;
    Json(AppStatusResponse {
        app_id,
        registered,
        running: port.is_some(),
        port,
    })
}

/// `DELETE /v1/apps/{app_id}` — unregister an app.
pub async fn uninstall_app(
    State(state): State<AppState>,
    Path(app_id): Path<String>,
) -> impl IntoResponse {
    let removed = state.inner.app_registry.unregister(&app_id).await;
    state.inner.app_port_map.remove(&app_id).await;
    if removed {
        (StatusCode::OK, Json(serde_json::json!({"status": "uninstalled"}))).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "app not found"}))).into_response()
    }
}

/// `ANY /app/{app_id}/*path` — reverse proxy to the app's local HTTP port.
pub async fn proxy_app(
    State(state): State<AppState>,
    Path((app_id, suffix)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let port = match state.inner.app_port_map.get(&app_id).await {
        Some(p) => p,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "app not running on this node"})),
            )
                .into_response();
        }
    };

    let path_suffix = format!("/{suffix}");
    let client = commonwealth_app::proxy::proxy_client();

    // Convert axum HeaderMap to reqwest HeaderMap.
    let mut req_headers = reqwest::header::HeaderMap::new();
    for (k, v) in &headers {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(v.as_bytes()),
        ) {
            req_headers.insert(name, val);
        }
    }

    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);

    match commonwealth_app::proxy::forward(
        &client,
        port,
        req_method,
        &path_suffix,
        req_headers,
        body,
    )
    .await
    {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body_bytes = resp.bytes().await.unwrap_or_default();
            (status, body_bytes).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}
