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
//! third-party client. The internal-port routes the flip gave to cw-rails
//! and serve answer the same way ([`internal_moved`]), and a path retired
//! with no new home names what replaced it ([`RETIRED`]).
//!
//! Localhost-only: any non-loopback caller gets `403 Forbidden`, same guard
//! as `mcp_router`.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};
use crate::state::AppState;

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

/// The mesh paths this daemon served until a cut that have no HTTP home
/// now, each with what replaced it. Each answers 410 with that text.
pub const RETIRED: &[(&str, &str)] = &[(
    "/v1/mesh/measurements",
    "measurements travel on cw-rails' ring journal now (pb-serve-placement): \
     `svrn mesh bench` publishes them and `svrn mesh plan` reads them",
)];

/// Build the mesh HTTP router. Merged into the daemon's client router next
/// to `mcp_router`.
pub fn mesh_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    let mut router = Router::new().merge(mesh_venues_router(Arc::clone(&daemon)));
    for path in MOVED_TO_RAILS {
        router = router.route(path, any(moved_to_rails));
    }
    for &(path, replaced_by) in RETIRED {
        router = router.route(
            path,
            any(move |_: LocalOnly| async move {
                tracing::debug!(target: "mesh", %path, "mesh_http: a retired path asked of svrn");
                gone(format!("{path} is retired: {replaced_by}"), None)
            }),
        );
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
    gone(
        format!(
            "{path} is served by cw-rails, the node's mesh endpoint, not by svrn: \
             call {rails}{path}"
        ),
        Some(format!("{rails}{path}")),
    )
}

/// The internal-port routes (`crate::server::internal_router`, `:9742`) this
/// daemon served until the flip, each now on cw-rails' own listener.
pub const INTERNAL_MOVED_TO_RAILS: &[&str] = &[
    "/internal/gossip",
    "/internal/join",
    "/internal/ring/sync",
    "/internal/ring/live",
    "/internal/ring/checkpoint/{ns}",
];

/// The internal-port routes serve took (sovereign-serve `rails_mesh`
/// `PEER_PREFIXES`), registered by serve with cw-rails on `cwth/http/0`.
pub const INTERNAL_MOVED_TO_SERVE: &[&str] = &["/internal/rpc-warm"];

/// Mount the internal routes the flip gave away on `router`, each a 410
/// naming its owner, so a caller of the old port reads where to go rather
/// than a 404 (FIVE_PROGRAMS §4 rule 3).
pub fn internal_moved(mut router: Router<AppState>) -> Router<AppState> {
    for path in INTERNAL_MOVED_TO_RAILS {
        router = router.route(path, any(internal_moved_to_rails));
    }
    for path in INTERNAL_MOVED_TO_SERVE {
        router = router.route(path, any(internal_moved_to_serve));
    }
    router
}

async fn internal_moved_to_rails(State(state): State<AppState>, uri: Uri) -> impl IntoResponse {
    let rails = &state.inner.node.rails_base;
    let path = uri.path();
    tracing::debug!(target: "mesh", %path, %rails, "mesh_http: a cw-rails internal path asked of svrn");
    gone(
        format!(
            "{path} is served by cw-rails, the node's mesh endpoint, not by svrn's \
             internal port: call {rails}{path}"
        ),
        Some(format!("{rails}{path}")),
    )
}

async fn internal_moved_to_serve(uri: Uri) -> impl IntoResponse {
    let serve = sovereign_turn_client::serve_self::default_serve_base();
    let path = uri.path();
    tracing::debug!(target: "mesh", %path, %serve, "mesh_http: a serve internal path asked of svrn");
    gone(
        format!(
            "{path} is served by serve, not by svrn's internal port: a member reaches \
             it through cw-rails on cwth/http/0, and on this host serve listens at {serve}"
        ),
        Some(format!("{serve}{path}")),
    )
}

/// The one shape of a 410 here: what happened to the path, and where it
/// went when it went somewhere.
fn gone(error: String, moved_to: Option<String>) -> axum::response::Response {
    let body = match moved_to {
        Some(to) => serde_json::json!({ "error": error, "moved_to": to }),
        None => serde_json::json!({ "error": error }),
    };
    (StatusCode::GONE, Json(body)).into_response()
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
