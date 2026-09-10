// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon serves the insight surface — `GET /v1/insights`,
//! `GET /v1/insights/search`, `DELETE /v1/insights/{id}`,
//! `POST /v1/insights/clip` (sv-surface rung 6, commit A).
//!
//! # Why this exists
//!
//! The desktop's insight commands (clip a passage while reading, list and
//! search the collection, delete a clip) have talked to an IN-PROCESS
//! `InsightService` opened beside the state store — one of the
//! attach-reachable deciders the sv-attach-pure-client bar counts. Rung 6
//! makes the attached desktop a client; this is the daemon half of that
//! conversion: the same `InsightService` the desktop builds, over the same
//! `sovereign.db` connection the daemon already owns, served on loopback.
//!
//! The service is not re-derived here. It is COMMISSIONED by the host that
//! owns the store (`daemon_cmd` for the standalone daemon, `state.rs` for
//! the desktop's in-process one) and handed to [`crate::daemon_services::
//! ServingCore`] — so there is one clip decider (the `InsightService` in
//! sovereign-core), and this file is only the door.
//!
//! # The wire shape strips the embedding, on purpose
//!
//! [`InsightEntry`] is the projection a client renders: every field of
//! `InsightNode` except `embedding` (raw floats the UI never draws) and
//! with `created_at` as RFC 3339 (the UI's existing spelling, carried over
//! from the desktop's local DTO). The projection lives HERE, beside the
//! route that serves it, following the `reading_http` pattern: the desktop
//! repoint (rung 6 commit D) serializes the same type, so in-process and
//! wire answers are byte-identical by construction rather than by review.
//!
//! # Loopback only
//!
//! Same two layers as `turn_http` and `reading_http`: the router-level
//! [`crate::loopback_guard::loopback_only`] middleware and a per-handler
//! peer check. An insight is a clip from this user's reading of this
//! host's corpora; it is not a peer-facing surface.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_contracts::types::{InsightPosition, InsightSinkState, InsightSource};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

/// Mount the insight surface. Built from `Arc<Self>` by `start_daemon`, like
/// the turn and reading routers — mounted unconditionally on serving
/// daemons; the handlers answer 503 with a named reason when no insight
/// service was commissioned (a mesh-admin daemon), rather than 404ing as an
/// unmounted route did.
pub fn insight_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/insights", get(list_insights))
        .route("/v1/insights/search", get(search_insights))
        .route("/v1/insights/{id}", delete(delete_insight))
        .route("/v1/insights/clip", post(clip_insight))
        .route("/v1/insights/sinks", get(sink_status))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

/// The wire projection of one insight node — what a client renders.
///
/// `embedding` is stripped (raw floats; the UI never draws them and a
/// leak would only bloat the payload), `created_at` is RFC 3339. This is
/// the ONE projection for the family: the desktop's repoint serializes
/// this same struct, so both transports emit identical bytes.
#[derive(Debug, Clone, Serialize)]
pub struct InsightEntry {
    pub id: String,
    pub clipped_text: String,
    pub message_id: String,
    pub paragraph_index: usize,
    pub source: InsightSource,
    pub position: Option<InsightPosition>,
    pub adjacent: Vec<String>,
    pub created_at: String,
    pub sink_state: InsightSinkState,
}

impl From<sovereign_contracts::types::InsightNode> for InsightEntry {
    fn from(n: sovereign_contracts::types::InsightNode) -> Self {
        Self {
            id: n.id.to_string(),
            clipped_text: n.clipped_text,
            message_id: n.message_id.to_string(),
            paragraph_index: n.paragraph_index,
            source: n.source,
            position: n.position,
            adjacent: n.adjacent,
            created_at: n.created_at.to_rfc3339(),
            sink_state: n.sink_state,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InsightListResponse {
    pub insights: Vec<InsightEntry>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

/// `POST /v1/insights/clip` — the wire form of `InsightService::clip`.
///
/// `source` and `position` arrive as the contracts-layer JSON the desktop
/// already produces for its in-process command (they are serde types in
/// `sovereign_contracts::types`), so a repoint serializes what it has
/// rather than re-deriving a shape.
#[derive(Debug, Deserialize)]
pub struct ClipRequest {
    pub clipped_text: String,
    pub message_id: String,
    pub paragraph_index: usize,
    pub source: InsightSource,
    #[serde(default)]
    pub position: Option<InsightPosition>,
}

#[derive(Debug, Serialize)]
pub struct ClipResponse {
    pub insight: InsightEntry,
}

/// `GET /v1/insights/sinks` — the ONE route D2 left owed.
///
/// Same shape the desktop's `get_sink_status` returns today
/// (`insight_commands.rs:152`): `any_connected` plus a per-sink list.
/// The desktop's list is a literal `vec![]` with a "populated when
/// Obsidian sink is added" note; this one is the registry's actual
/// contents, because `InsightSink` already carries `id()` and
/// `display_name()` and there was never anything to invent. That is
/// the defect this route closes, not a bonus: on an attached boot the
/// command reads THIS process's registry, which is empty, so
/// `any_connected` was false regardless of the daemon's sinks.
#[derive(Debug, Serialize)]
pub struct SinkStatusResponse {
    pub any_connected: bool,
    pub sinks: Vec<SinkInfo>,
}

/// One registered sink. `connected` is probed per sink, so a settings
/// pane can say WHICH one is down rather than only that something is.
#[derive(Debug, Serialize)]
pub struct SinkInfo {
    pub id: String,
    pub display_name: String,
    pub connected: bool,
}

async fn list_insights(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<ListQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(service) = daemon.insight_service() else {
        return service_unavailable("this daemon serves no insight surface (no insight service)");
    };
    let limit = params.limit.unwrap_or(50);
    match service.store.list(limit).await {
        Ok(nodes) => Json(InsightListResponse {
            insights: nodes.into_iter().map(InsightEntry::from).collect(),
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn search_insights(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<SearchQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(service) = daemon.insight_service() else {
        return service_unavailable("this daemon serves no insight surface (no insight service)");
    };
    let Some(q) = params.q.filter(|q| !q.is_empty()) else {
        return bad_request("the q parameter is required and must not be empty");
    };
    match service.store.search_text(&q, 20).await {
        Ok(nodes) => Json(InsightListResponse {
            insights: nodes.into_iter().map(InsightEntry::from).collect(),
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn delete_insight(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(service) = daemon.insight_service() else {
        return service_unavailable("this daemon serves no insight surface (no insight service)");
    };
    let Ok(id) = uuid::Uuid::parse_str(&id) else {
        return bad_request("insight id must be a UUID");
    };
    match service.store.delete(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn clip_insight(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<ClipRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(service) = daemon.insight_service() else {
        return service_unavailable("this daemon serves no insight surface (no insight service)");
    };
    let Ok(message_id) = uuid::Uuid::parse_str(&body.message_id) else {
        return bad_request("message_id must be a UUID");
    };
    match service
        .clip(
            &body.clipped_text,
            message_id,
            body.paragraph_index,
            body.source,
            body.position,
        )
        .await
    {
        Ok(node) => Json(ClipResponse {
            insight: InsightEntry::from(node),
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// GET `/v1/insights/sinks` — every registered insight sink and
/// whether it is reachable right now.
///
/// `any_connected` is the registry's own fold, not a re-derivation over
/// the list below: one decider for "is anything connected" (ARCH
/// §10.6), and it stays true even if a future sink reports connectivity
/// some way the per-row probe does not.
///
/// An EMPTY list with `any_connected: false` is the correct answer for
/// a daemon nobody configured a vault on — a successful read, not a
/// 404. "No insight service at all" is the different fact, and that is
/// the named 503 the other four handlers give.
async fn sink_status(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(service) = daemon.insight_service() else {
        return service_unavailable("this daemon serves no insight surface (no insight service)");
    };
    let any_connected = service.sinks.any_connected().await;
    let mut sinks = Vec::new();
    for sink in service.sinks.iter() {
        sinks.push(SinkInfo {
            id: sink.id().to_string(),
            display_name: sink.display_name().to_string(),
            connected: sink.is_connected().await,
        });
    }
    tracing::debug!(
        any_connected,
        registered = sinks.len(),
        "insight_http: sink status served",
    );
    Json(SinkStatusResponse {
        any_connected,
        sinks,
    })
    .into_response()
}

fn bad_request(reason: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}

fn internal_error(reason: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}

fn service_unavailable(reason: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}
