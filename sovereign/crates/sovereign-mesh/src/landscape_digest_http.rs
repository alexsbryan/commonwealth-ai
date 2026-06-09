// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/knowledge/landscape_digest` — daemon-side endpoint that
//! lets an attached desktop fetch the prompt-spliced landscape
//! digest blocks the daemon's own `KnowledgeViewManager` would
//! produce.
//!
//! Mounted into the daemon's client router at the same `:9741`
//! listener as `/v1/knowledge/search`. Localhost-only via the
//! `loopback_guard` middleware (defense in depth on top of the
//! per-handler check), same shape as `mesh_http`.
//!
//! The endpoint exists because the desktop in attach mode no longer
//! constructs its own `KnowledgeViewManager` (see
//! `AppState::is_attach_mode` in sovereign-desktop) — the daemon
//! owns enrichment, so the desktop must pull the assembled digest
//! over HTTP rather than splice locally.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use commonwealth_inference::oicp::{
    LandscapeDigestEntry, LandscapeDigestRequest, LandscapeDigestResponse,
};

use sovereign_tools::knowledge_view::KnowledgeViewManager;

use crate::loopback_guard::enforce_localhost;

/// Build the landscape-digest router. Caller must hand an
/// `Arc<KnowledgeViewManager>` cloned from the manager that
/// owns the daemon's enrichment state — the manager's view
/// registry is what the handler reads.
pub fn landscape_digest_router(manager: Arc<KnowledgeViewManager>) -> Router {
    Router::new()
        .route(
            "/v1/knowledge/landscape_digest",
            post(landscape_digest_handler),
        )
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(manager))
}

async fn landscape_digest_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(manager): Extension<Arc<KnowledgeViewManager>>,
    body: Option<Json<LandscapeDigestRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let req = body.map(|Json(b)| b).unwrap_or_default();

    let digests = manager
        .compute_digests(
            req.active_skill.as_deref(),
            req.active_is_local_only,
            &req.conversation_messages,
        )
        .await;
    let entries: Vec<LandscapeDigestEntry> = digests
        .into_iter()
        .map(|d| LandscapeDigestEntry {
            view_id: d.view_id,
            body: d.body,
        })
        .collect();
    (
        StatusCode::OK,
        Json(LandscapeDigestResponse { digests: entries }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `LandscapeDigestRequest` deserialises from `{}` — the simplest
    /// valid request. Pinned because the desktop's HTTP client
    /// constructs the request with all fields default.
    #[test]
    fn empty_body_deserialises() {
        let req: LandscapeDigestRequest = serde_json::from_str("{}").unwrap();
        assert!(req.active_skill.is_none());
        assert!(req.conversation_messages.is_empty());
    }

    #[test]
    fn full_body_round_trips() {
        let req = LandscapeDigestRequest {
            active_skill: Some("inner-work".into()),
            active_is_local_only: true,
            conversation_messages: vec!["hello".into(), "world".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: LandscapeDigestRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.active_skill.as_deref(), Some("inner-work"));
        assert_eq!(parsed.conversation_messages.len(), 2);
    }

    #[test]
    fn response_with_zero_digests_serializes() {
        let resp = LandscapeDigestResponse { digests: vec![] };
        let json = serde_json::to_string(&resp).unwrap();
        // The shape must include the digests field even when empty
        // so the desktop client doesn't have to special-case None.
        assert!(json.contains("\"digests\""));
    }
}
