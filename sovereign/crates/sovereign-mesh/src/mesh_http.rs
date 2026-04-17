//! HTTP surface for mesh mutation. Lets an out-of-process client
//! (most notably the desktop app, when it detects that a CLI-started
//! daemon already owns `:9741`) drive `create / join / rotate / leave`
//! without reimplementing the state machine.
//!
//! Before this module, mesh mutations were Rust-only via
//! `EmbeddedDaemon::{create_mesh, join_mesh, stop}` — which meant a
//! second process couldn't change mesh state without attempting to
//! start its own daemon (silent port collision, no mesh parity).
//!
//! Routes mount under `/v1/mesh/*` on the same `:9741` listener as
//! `/v1/chat/completions` and `/mcp`. Localhost-only: any non-loopback
//! caller gets `403 Forbidden`, same guard as `mcp_router`. The
//! endpoints are deliberately symmetric with the `sovereign mesh …`
//! CLI subcommands.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::daemon::EmbeddedDaemon;

/// Build the mesh HTTP router. Merged into the daemon's client router
/// next to `mcp_router`. Call once at `start_daemon` time and hand the
/// same `Arc<EmbeddedDaemon>` that `start_daemon` owns internally.
pub fn mesh_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/mesh/status", get(mesh_status))
        .route("/v1/mesh/create", post(mesh_create))
        .route("/v1/mesh/join", post(mesh_join))
        .route("/v1/mesh/rotate", post(mesh_rotate))
        .route("/v1/mesh/leave", post(mesh_leave))
        .layer(Extension(daemon))
}

/// Request body for `POST /v1/mesh/create`. Both fields default so a
/// bare `POST` with empty body is valid — mirrors `sovereign mesh create`
/// with no args.
#[derive(Debug, Deserialize, Default)]
pub struct CreateRequest {
    /// Human-readable mesh name. Defaults to `"<host>'s Mesh"`.
    #[serde(default)]
    pub name: Option<String>,
    /// Node display name. Defaults to the machine's hostname.
    #[serde(default)]
    pub node_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateResponse {
    pub mesh_name: String,
    pub join_key: String,
    pub join_link: String,
}

/// Request body for `POST /v1/mesh/join`. Accepts any of the three
/// forms the CLI accepts: bare `cwth-…` key, `https://sovereign.dev/join/…`
/// URL, or `sovereign://join/…` deep link.
#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub key_or_url: String,
    #[serde(default)]
    pub node_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JoinResponse {
    pub mesh_name: String,
    pub node_id: String,
}

#[derive(Debug, Serialize)]
pub struct RotateResponse {
    pub mesh_name: String,
    pub join_key: String,
}

/// Shape returned by `GET /v1/mesh/status`. Flat and JSON-friendly so
/// desktop UIs can render it without a client-side wrapper type.
/// Field names are aligned with the existing `MeshState` the desktop
/// already knows how to render — an HTTP client can round-trip through
/// this DTO without losing information.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub running: bool,
    pub mesh_name: Option<String>,
    pub members_online: usize,
    pub members_total: usize,
    pub members: Vec<MemberDto>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemberDto {
    pub node_id: String,
    pub name: String,
    pub is_self: bool,
    /// `"online"` | `"busy"` | `"away"` | `"offline"` — matches the
    /// `MemberStatus` serde rename in `crate::types`.
    pub status: String,
}

// ─── Localhost guard ──────────────────────────────────────────────

fn enforce_localhost(addr: &SocketAddr) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "local-only" })),
        ))
    }
}

fn default_node_name(override_name: Option<String>) -> String {
    override_name.unwrap_or_else(|| {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "sovereign-node".to_string())
    })
}

// ─── Handlers ────────────────────────────────────────────────────

/// `GET /v1/mesh/status` — read-only snapshot for UI polling.
async fn mesh_status(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }

    let running = daemon.is_running().await;
    let Some(s) = daemon.mesh_state().await else {
        // Running but no mesh — e.g. the daemon started solo and the
        // user hasn't run `mesh create` yet. Empty but valid payload
        // keeps the UI's empty-state rendering happy.
        return (
            StatusCode::OK,
            Json(serde_json::to_value(StatusResponse {
                running,
                mesh_name: None,
                members_online: 0,
                members_total: 0,
                members: vec![],
            }).unwrap()),
        )
            .into_response();
    };

    // Map MemberStatus to its serde-renamed variant. `MeshMember`
    // already owns its `node_id` as a String (set by `mesh_state`), so
    // we don't need to Debug-format a NodeId byte array.
    let members: Vec<MemberDto> = s
        .members
        .iter()
        .map(|m| MemberDto {
            node_id: m.node_id.clone(),
            name: m.name.clone(),
            is_self: m.is_self,
            status: match m.status {
                crate::types::MemberStatus::Online => "online",
                crate::types::MemberStatus::Busy => "busy",
                crate::types::MemberStatus::Away => "away",
                crate::types::MemberStatus::Offline => "offline",
            }
            .to_string(),
        })
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::to_value(StatusResponse {
            running,
            mesh_name: Some(s.status.name),
            members_online: s.status.members_online,
            members_total: s.status.members_total,
            members,
        }).unwrap()),
    )
        .into_response()
}

/// `POST /v1/mesh/create` — promote the solo daemon to a joinable mesh
/// and return the shareable invite.
async fn mesh_create(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: Option<Json<CreateRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let node_name = default_node_name(req.node_name);
    let mesh_name = req
        .name
        .unwrap_or_else(|| format!("{node_name}'s Mesh"));

    match daemon.create_mesh(&mesh_name, &node_name).await {
        Ok(result) => (
            StatusCode::OK,
            Json(serde_json::to_value(CreateResponse {
                mesh_name: result.mesh_name,
                join_key: result.join_key,
                join_link: result.join_link,
            }).unwrap()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /v1/mesh/join` — join an existing mesh by key or URL.
async fn mesh_join(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<JoinRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    // Accept bare key, https URL, or sovereign:// deep link — matches
    // what the CLI's `sovereign mesh join` takes.
    let link = match crate::deep_link::parse_join_argument(&req.key_or_url) {
        Some(l) => l,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "key_or_url must be a bare cwth-… key, an https://sovereign.dev/join/… URL, or a sovereign://join/… deep link"
                })),
            )
                .into_response();
        }
    };
    let node_name = default_node_name(req.node_name);

    match daemon.join_mesh(&link, &node_name).await {
        Ok(result) => (
            StatusCode::OK,
            Json(serde_json::to_value(JoinResponse {
                mesh_name: result.mesh_name,
                node_id: result.node_id,
            }).unwrap()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /v1/mesh/rotate` — regenerate the join key for an existing
/// mesh. Delegates to `persist::rotate_join_key`; the daemon's
/// in-memory hash is refreshed on next restart (the handler surfaces
/// this in the response so the caller can decide whether to also hit
/// `/v1/admin/reload`).
async fn mesh_rotate(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    // Pull data_dir via a new accessor — rotate_join_key works on disk
    // state, independent of the running daemon.
    let data_dir = daemon.data_dir().to_path_buf();
    match crate::persist::rotate_join_key(&data_dir) {
        Ok(Some(rotated)) => (
            StatusCode::OK,
            Json(serde_json::to_value(RotateResponse {
                mesh_name: rotated.mesh_name,
                join_key: rotated.join_key,
            }).unwrap()),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no mesh to rotate" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /v1/mesh/leave` — stop the daemon and clear persisted mesh
/// state. Mirrors the desktop "Leave mesh" button.
async fn mesh_leave(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r.into_response();
    }
    match daemon.stop().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmbeddedDaemon;
    use tempfile::TempDir;

    /// Stand up the mesh HTTP router over a no-mesh daemon bound to
    /// an ephemeral localhost port. Returns `(daemon_arc, base_url,
    /// _tmp)` — hold the tempdir so it isn't cleaned up mid-test.
    async fn spawn_test_router() -> (Arc<EmbeddedDaemon>, String, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Arc::new(EmbeddedDaemon::new(tmp.path().to_path_buf()));
        let app = mesh_router(Arc::clone(&daemon));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (daemon, format!("http://{addr}"), tmp)
    }

    #[tokio::test]
    async fn status_returns_empty_when_no_mesh() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let resp = client.get(format!("{base}/v1/mesh/status")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["running"], false);
        assert_eq!(body["member_count"].as_u64().unwrap_or(0), 0);
    }

    #[tokio::test]
    async fn create_and_status_round_trip() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "test mesh", "node_name": "alice" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["mesh_name"], "test mesh");
        assert!(body["join_key"].as_str().unwrap().starts_with("cwth-"));
        assert!(body["join_link"].as_str().unwrap().contains("sovereign://"));

        // Status should now report running + one member.
        let resp = client.get(format!("{base}/v1/mesh/status")).send().await.unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["running"], true);
        assert_eq!(body["mesh_name"], "test mesh");
        assert_eq!(body["members_total"], 1);
    }

    #[tokio::test]
    async fn create_fails_when_mesh_already_exists() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let _ = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "first" }))
            .send()
            .await
            .unwrap();
        let resp = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "second" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409, "second create must conflict");
    }

    #[tokio::test]
    async fn rotate_after_create_changes_join_key_hash() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();

        let create: serde_json::Value = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "m" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let original_key = create["join_key"].as_str().unwrap().to_string();

        let rotate: serde_json::Value = client
            .post(format!("{base}/v1/mesh/rotate"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let rotated_key = rotate["join_key"].as_str().unwrap();
        assert!(rotated_key.starts_with("cwth-"));
        assert_ne!(original_key, rotated_key);
    }

    #[tokio::test]
    async fn rotate_without_mesh_returns_404() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let resp = client.post(format!("{base}/v1/mesh/rotate")).send().await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn join_rejects_unparseable_input() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/v1/mesh/join"))
            .json(&serde_json::json!({ "key_or_url": "not a valid key" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }
}

