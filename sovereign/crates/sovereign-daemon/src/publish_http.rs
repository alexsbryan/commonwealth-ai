// SPDX-License-Identifier: AGPL-3.0-or-later
//! The publishing half, loopback-only: what THIS node offers the house, and
//! the claims that keep it honest.
//!
//! `/v1/mesh/media` and `/v1/mesh/app` are the viewer surfaces — what
//! everybody else publishes. These four are the other direction, and they are
//! the ones a housemate's own process calls:
//!
//! ```text
//! GET    /v1/mesh/publish                    what am I publishing
//! POST   /v1/mesh/publish                    {name, port, ttl_secs?} -> a claim
//! POST   /v1/mesh/publish/{claim_id}/renew   {ttl_secs?} -> the same claim, later
//! DELETE /v1/mesh/publish/{claim_id}         stop
//! ```
//!
//! **A claim names a PORT, not an address.** The claimed tier's guarantee is
//! that a registration cannot outlive the process that owns it, and a process
//! on this box does not own the lifetime of something on another one. An
//! always-on service at a fixed address is the config tier's case, and
//! `[iroh.apps]` still takes a full `host:port` for exactly that.
//!
//! **The claim id is not a credential.** Every route here is loopback-only,
//! and any local process that could present one could have taken the claim
//! itself. It identifies a claim so a release cannot unpublish somebody
//! else's app; it authorises nothing.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, Json, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use commonwealth_media::apps::{PublishRefusal, DEFAULT_CLAIM_TTL};
use serde::{Deserialize, Serialize};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::LocalOnly;

/// `POST /v1/mesh/publish` body.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimRequest {
    /// The name housemates reach it by, as the first path segment.
    pub name: String,
    /// The loopback port it answers on.
    pub port: u16,
    /// How long the claim survives without a renew. Absent takes
    /// [`DEFAULT_CLAIM_TTL`]; anything past the registry's cap is clamped to
    /// it and the answer says what was granted.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// `POST /v1/mesh/publish/{claim_id}/renew` body. An empty body is valid.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RenewRequest {
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// `GET /v1/mesh/publish` — the two tiers in one list.
#[derive(Debug, Clone, Serialize)]
pub struct PublishingResponse {
    pub apps: Vec<commonwealth_media::PublishedApp>,
    /// This node's `[iroh] media_origin`, so one call answers "what am I
    /// offering the house" rather than two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_origin: Option<String>,
}

fn ttl_of(secs: Option<u64>) -> Duration {
    secs.map(Duration::from_secs).unwrap_or(DEFAULT_CLAIM_TTL)
}

/// A refusal keeps its own shape: a name collision is a 409 (the request was
/// well-formed and the world said no), an unusable name a 400, an unknown
/// claim a 404. Collapsing these into one code is how a runner ends up
/// retrying a name it can never have.
fn refusal_response(e: PublishRefusal) -> axum::response::Response {
    let code = match e {
        PublishRefusal::BadName(_) => StatusCode::BAD_REQUEST,
        PublishRefusal::NameTaken { .. } => StatusCode::CONFLICT,
        PublishRefusal::NoSuchClaim(_) => StatusCode::NOT_FOUND,
    };
    (code, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

/// `GET /v1/mesh/publish`
pub async fn publishing(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    let media_origin = daemon.configured_media_origin().await;
    (
        StatusCode::OK,
        Json(PublishingResponse {
            apps: daemon.published_apps().listing(),
            media_origin,
        }),
    )
        .into_response()
}

/// `POST /v1/mesh/publish`
pub async fn publish_app(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<ClaimRequest>,
) -> impl IntoResponse {
    let addr = ([127, 0, 0, 1], req.port).into();
    match daemon
        .published_apps()
        .claim(&req.name, addr, ttl_of(req.ttl_secs))
    {
        Ok(claim) => (StatusCode::OK, Json(claim)).into_response(),
        Err(e) => refusal_response(e),
    }
}

/// `POST /v1/mesh/publish/{claim_id}/renew`
pub async fn renew_app(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(claim_id): Path<String>,
    body: Option<Json<RenewRequest>>,
) -> impl IntoResponse {
    let ttl = ttl_of(body.and_then(|Json(b)| b.ttl_secs));
    match daemon.published_apps().renew(&claim_id, ttl) {
        Ok(claim) => (StatusCode::OK, Json(claim)).into_response(),
        Err(e) => refusal_response(e),
    }
}

/// `DELETE /v1/mesh/publish/{claim_id}`
pub async fn unpublish_app(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(claim_id): Path<String>,
) -> impl IntoResponse {
    match daemon.published_apps().release(&claim_id) {
        Ok(name) => (
            StatusCode::OK,
            Json(serde_json::json!({ "released": name })),
        )
            .into_response(),
        Err(e) => refusal_response(e),
    }
}
