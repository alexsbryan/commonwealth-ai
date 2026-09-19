// SPDX-License-Identifier: AGPL-3.0-or-later
//! The insight surface — `/v1/insights…` (sv-surface rung 6, D2, D8).
//!
//! List, search, clip, delete, sink status and the `by-id` read, over the
//! `InsightService` the HOST commissions beside the state store and hands to
//! [`crate::daemon_services::ServingCore`]. Nothing is re-derived here: there
//! is one clip decider, in sovereign-core, and this file is the door.
//! [`InsightEntry`] is the projection a client renders — every `InsightNode`
//! field except `embedding`, `created_at` as RFC 3339 — and it lives beside
//! the route, so in-process and wire answers agree by construction.
//!
//! Loopback posture is `reading_http`'s, unchanged: an insight is a clip from
//! this user's reading of this host's corpora, never peer-facing.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_contracts::types::{InsightPosition, InsightSinkState, InsightSource};

use crate::daemon::EmbeddedDaemon;
use sovereign_core::insight::InsightService;

use crate::http_response::{bad_request, internal_error, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

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
        .route("/v1/insights/by-id", post(insights_by_id))
        .localhost_only_with(daemon)
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

/// `POST /v1/insights/by-id` — the ids to fetch.
///
/// A POST because the input is a LIST, and `serde_urlencoded` cannot take
/// a sequence — the same call `atlas_http`'s atom filter and
/// `notes_http`'s query already make, rather than inventing a second
/// comma-joining encoding for one route (ARCH §10.6).
#[derive(Debug, Deserialize)]
pub struct ByIdRequest {
    pub ids: Vec<String>,
}

/// The nodes that resolved, and the ids that did not.
///
/// `missing` is the point of the shape. `InsightStore::list_by_ids`
/// silently drops ids that name no live row — deleted, or never
/// existed — so a caller comparing lengths learns only that something
/// went missing, not which. A short list with no `missing` array would be
/// an absence the caller has to infer (ARCH §18.3). Empty on the happy
/// path, and an explicit empty array, never an absent key.
#[derive(Debug, Serialize)]
pub struct ByIdResponse {
    pub insights: Vec<InsightEntry>,
    pub missing: Vec<String>,
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<ListQuery>,
) -> Result<Response, Absence> {
    let service = service_for(&daemon)?;
    let limit = params.limit.unwrap_or(50);
    Ok(match service.store.list(limit).await {
        Ok(nodes) => Json(InsightListResponse {
            insights: nodes.into_iter().map(InsightEntry::from).collect(),
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    })
}

async fn search_insights(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(params): Query<SearchQuery>,
) -> Result<Response, Absence> {
    let service = service_for(&daemon)?;
    let Some(q) = params.q.filter(|q| !q.is_empty()) else {
        return Ok(bad_request(
            "the q parameter is required and must not be empty",
        ));
    };
    Ok(match service.store.search_text(&q, 20).await {
        Ok(nodes) => Json(InsightListResponse {
            insights: nodes.into_iter().map(InsightEntry::from).collect(),
        })
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    })
}

async fn delete_insight(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let service = service_for(&daemon)?;
    let Ok(id) = uuid::Uuid::parse_str(&id) else {
        return Ok(bad_request("insight id must be a UUID"));
    };
    Ok(match service.store.delete(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(&e.to_string()),
    })
}

async fn clip_insight(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<ClipRequest>,
) -> Result<Response, Absence> {
    let service = service_for(&daemon)?;
    let Ok(message_id) = uuid::Uuid::parse_str(&body.message_id) else {
        return Ok(bad_request("message_id must be a UUID"));
    };
    Ok(
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
        },
    )
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let service = service_for(&daemon)?;
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
    Ok(Json(SinkStatusResponse {
        any_connected,
        sinks,
    })
    .into_response())
}

/// POST `/v1/insights/by-id` — the read `explore_insights` still does
/// in-process (`insight_commands.rs:191`, `service.store.list_by_ids`),
/// which 19946f99a named as the one insight read with no route.
///
/// The ORDER of `insights` is the store's, not the request's: the query
/// is one `WHERE id IN (...)` and sqlite owes no ordering. A caller that
/// needs its own order has the ids and can index by them; a route that
/// re-sorted here would be inventing a guarantee the store does not make.
///
/// An unparseable uuid is a 400 naming WHICH one — one bad id in a batch
/// of thirty is otherwise a mystery.
async fn insights_by_id(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<ByIdRequest>,
) -> Result<Response, Absence> {
    let service = service_for(&daemon)?;
    let mut ids = Vec::with_capacity(body.ids.len());
    for raw in &body.ids {
        match uuid::Uuid::parse_str(raw) {
            Ok(id) => ids.push(id),
            Err(e) => return Ok(bad_request(&format!("`{raw}` is not an insight id: {e}"))),
        }
    }
    // An empty request is a successful empty answer, not an error: a
    // selection of zero passages is a state a UI reaches by deselecting.
    Ok(match service.store.list_by_ids(&ids).await {
        Ok(nodes) => {
            let found: std::collections::HashSet<String> =
                nodes.iter().map(|n| n.id.to_string()).collect();
            let missing: Vec<String> = ids
                .iter()
                .map(|i| i.to_string())
                .filter(|i| !found.contains(i))
                .collect();
            tracing::debug!(
                requested = ids.len(),
                returned = nodes.len(),
                missing = missing.len(),
                "insight_http: nodes fetched by id",
            );
            Json(ByIdResponse {
                insights: nodes.into_iter().map(InsightEntry::from).collect(),
                missing,
            })
            .into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    })
}

/// The daemon's own `InsightService`. ONE lookup site, so no handler can
/// reach a different clip store than `POST /v1/insights/clip` writes to —
/// six handlers spelled the same `let Some(…) else { return 503 }` before.
fn service_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<InsightService>, Absence> {
    daemon.insight_service().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable("this daemon serves no insight surface (no insight service)")
    })
}
