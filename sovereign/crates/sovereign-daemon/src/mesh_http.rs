// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's `/v1/mesh/*` surface after the flip (pb-mesh-exit-transport).
//!
//! cw-rails is the node's one mesh endpoint: it serves the roster, the
//! membership verbs, media, apps, offers, publishing and the mesh KV, at the
//! same paths and bodies this daemon used to (commonwealth-rails api.rs and
//! membership.rs). svrn keeps one mesh read of its own, `GET /v1/mesh/venues`
//! (its inference venues over cw-rails' roster), and answers every path it
//! used to serve for cw-rails with a named pointer to cw-rails' base (410) —
//! never a 404 that reads as "no such thing" (FIVE_PROGRAMS §4 rule 3). The
//! in-repo clients dial cw-rails' base directly; the pointer is for a
//! third-party client.
//!
//! Localhost-only: any non-loopback caller gets `403 Forbidden`, same guard
//! as `mcp_router`.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// The paths this daemon served for the mesh until the flip, each now
/// cw-rails'. One list, read by the router and by the duplicate-route test.
pub const MOVED_TO_RAILS: &[&str] = &[
    "/v1/mesh/status",
    "/v1/mesh/create",
    "/v1/mesh/join",
    "/v1/mesh/join/preview",
    "/v1/mesh/rotate",
    "/v1/mesh/switch",
    "/v1/mesh/forget",
    "/v1/mesh/leave",
    "/v1/mesh/forget-member",
    "/v1/mesh/relay-candidates",
    "/v1/mesh/media",
    "/v1/mesh/app",
    "/v1/mesh/offers",
    "/v1/mesh/fanout",
    "/v1/mesh/media/fanout",
    "/v1/mesh/publish",
    "/v1/mesh/publish/{claim_id}",
    "/v1/mesh/publish/{claim_id}/renew",
    "/v1/mesh/kv/entry",
    "/v1/mesh/kv/entries",
];

/// Build the mesh HTTP router. Merged into the daemon's client router next
/// to `mcp_router`.
pub fn mesh_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    let mut router = Router::new().merge(mesh_venues_router(Arc::clone(&daemon)));
    for path in MOVED_TO_RAILS {
        router = router.route(path, any(moved_to_rails));
    }
    // Router-level loopback guard — defense in depth on top of the
    // per-handler `LocalOnly` checks.
    router.localhost_only_with(daemon)
}

/// `GET /v1/mesh/venues` alone. `mesh_router` merges it, and a mesh-admin
/// daemon (which mounts no host surface) serves it by itself, so the setup
/// wizard's join child answers the one read the wizard polls
/// (five-programs-62).
pub fn mesh_venues_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/mesh/venues", get(mesh_venues))
        .localhost_only_with(daemon)
}

/// A path cw-rails serves now: 410 naming where, from the rails base this
/// daemon resolves (`rails_client::resolve_rails_base`, the one reader).
async fn moved_to_rails(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    uri: Uri,
) -> impl IntoResponse {
    let rails = daemon.rails_base().await;
    let path = uri.path();
    tracing::debug!(target: "mesh", %path, %rails, "mesh_http: a cw-rails path asked of svrn");
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "error": format!(
                "{path} is served by cw-rails, the node's mesh endpoint, not by svrn: \
                 call {rails}{path}"
            ),
            "moved_to": format!("{rails}{path}"),
        })),
    )
        .into_response()
}

/// `GET /v1/mesh/venues` — `EmbeddedDaemon::peer_inference_endpoints` on
/// the wire: online dialable peers with their transport-resolved client base
/// URLs. A stopped or solo daemon answers an empty list.
async fn mesh_venues(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    let venues: Vec<sovereign_contracts::daemon_wire::PeerVenue> = daemon
        .peer_inference_endpoints()
        .await
        .into_iter()
        .map(|v| sovereign_contracts::daemon_wire::PeerVenue {
            node_id: v.node_id.to_hex(),
            name: v.name,
            base_urls: v.base_urls,
        })
        .collect();
    tracing::debug!(count = venues.len(), "mesh_http: venues");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "venues": venues })),
    )
        .into_response()
}
