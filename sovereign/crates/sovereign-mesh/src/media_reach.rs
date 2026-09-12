// SPDX-License-Identifier: AGPL-3.0-or-later
//! Federated media, the viewer half: a member asks this daemon for a local
//! port that reaches a peer's `[iroh] media_origin`, and points a player at
//! it.
//!
//! The holder half (`iroh_access::AcceptorRoutes::media`, `MEDIA_ALPN`)
//! forwards a MEMBER's dial to the origin bound on its loopback and refuses
//! a stranger's. What was missing is the ask: nothing on this side minted a
//! bridge over that ALPN, so the only way to reach a peer's library was the
//! bench binary. `GET /v1/mesh/media?peer=<name-or-id>` is that ask, and
//! `svrn mesh media <peer>` prints what it returns.
//!
//! Nothing here parses HTTP or knows what a title is. The transport already
//! caches one bridge per `(peer, ALPN)` and retargets it in place when the
//! peer's dial info moves, so the URL this hands out stays valid for the life
//! of the daemon — a player can hold it. What it refuses, it refuses by name:
//! a missing member, an ambiguous prefix, an offline peer, a peer with no
//! iroh identity, a mesh with no iroh path to it. None of those is a URL that
//! will not answer (ARCH §18.3).
//!
//! Split out of `daemon.rs` and `mesh_http.rs` rather than added to them —
//! both are past ARCH §3.1's ceiling, and this is one concern with a seam of
//! its own, the same shape as `roster_repair`.
//!
//! Since 2026-09-11 (cw-lift D1) the decisions themselves are
//! `commonwealth-media`'s and only the route and the daemon glue are here, so
//! the inference daemon and the package-only rails daemon answer these
//! questions with one implementation (ARCH §10.6).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::ids::NodeId;
use serde::Deserialize;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

use commonwealth_media::PeerTransportPath;
/// The decisions themselves live in `commonwealth-media` (moved 2026-09-11,
/// cw-lift D1): the roster projection, the by-name pick with its refusals, and
/// the loopback URL. Re-exported under their old paths so `mesh_http`,
/// `daemon`, `media_fanout`, the CLI and the tests keep naming them here,
/// while a package-only rails daemon composes exactly the same code
/// (ARCH §10.6).
pub use commonwealth_media::{
    offering_members, pick_member, player_url, MediaCandidate, MediaOffer, MediaReach,
    MediaReachRefusal,
};

impl EmbeddedDaemon {
    /// The members offering an origin of `kind`, with the live path to each.
    /// The read behind `svrn mesh media` / `svrn mesh app` with no peer;
    /// nothing is dialed.
    pub async fn origin_offers(
        &self,
        kind: OriginKind,
    ) -> Result<Vec<MediaOffer>, MediaReachRefusal> {
        let app_state = self.app_state().await.ok_or(MediaReachRefusal::NoMesh)?;
        let self_id = app_state.self_node_id();
        let roster = {
            let mesh = app_state.inner.mesh.read().await;
            commonwealth_media::roster_of(&mesh)
        };
        Ok(commonwealth_media::offers(
            self_id,
            &roster,
            &self.peer_paths().await,
            kind,
        ))
    }

    /// Mint (or reuse) the loopback bridge to `query`'s media origin and
    /// return the URL a player is pointed at.
    ///
    /// Not TCP-probed, for the same reason `bridge_rpc_endpoint` is not: the
    /// loopback listener accepts instantly whether or not the peer is
    /// dialable, so a connect probe is a false positive by construction. The
    /// peer's gossip status is the liveness evidence, and the CLI does one
    /// real `GET /` through the bridge so the person sees an HTTP status
    /// rather than a port.
    pub async fn origin_reach(
        &self,
        query: &str,
        kind: OriginKind,
    ) -> Result<MediaReach, MediaReachRefusal> {
        let app_state = self.app_state().await.ok_or(MediaReachRefusal::NoMesh)?;
        let self_id = app_state.self_node_id();
        // Cloned out before any await: nothing here holds the mesh lock
        // across a dial.
        let roster = {
            let mesh = app_state.inner.mesh.read().await;
            commonwealth_media::roster_of(&mesh)
        };
        let paths = self.peer_paths().await;
        commonwealth_media::reach(
            self_id,
            &roster,
            query,
            &app_state.peer_transport(),
            &paths,
            kind,
        )
        .await
    }

    /// The live iroh path per peer, as the media reads want it: the daemon's
    /// snapshot flattened to the pairs `commonwealth-media` takes. A member
    /// the endpoint holds no record of is simply absent.
    pub(crate) async fn peer_paths(&self) -> Vec<(NodeId, PeerTransportPath)> {
        self.iroh_transport_snapshot()
            .await
            .into_iter()
            .filter_map(|p| p.path.map(|path| (p.node_id, path)))
            .collect()
    }
}

/// Query for `GET /v1/mesh/media`.
#[derive(Debug, Deserialize)]
pub struct MediaQuery {
    /// Member name or node-id prefix (≥4 chars), as `svrn mesh status` shows.
    /// Absent: list the members that offer a media origin instead.
    #[serde(default)]
    pub peer: Option<String>,
}

/// `GET /v1/mesh/media?peer=<name-or-id>` — the loopback URL that reaches
/// that member's `[iroh] media_origin`. Loopback-only like every `/v1/mesh/*`
/// route: the URL it returns is only usable from this machine anyway.
pub async fn mesh_media(
    caller: ConnectInfo<SocketAddr>,
    daemon: Extension<Arc<EmbeddedDaemon>>,
    q: Query<MediaQuery>,
) -> impl IntoResponse {
    origin_route(caller, daemon, q, OriginKind::Media).await
}

/// `GET /v1/mesh/app?peer=<name-or-id>` — the loopback BASE url that reaches
/// that member's published apps. The app itself is named by the first path
/// segment under it, so `<base>/chores/tasks` is the chore app's `/tasks`.
///
/// One route beside media rather than one route with a `kind=` parameter: the
/// two are different trust classes on different ALPNs, and a caller that can
/// flip between them with a query string reads as one capability when it is
/// two (the same reason `APP_ALPN` is not a path convention on media's).
pub async fn mesh_app(
    caller: ConnectInfo<SocketAddr>,
    daemon: Extension<Arc<EmbeddedDaemon>>,
    q: Query<MediaQuery>,
) -> impl IntoResponse {
    origin_route(caller, daemon, q, OriginKind::App).await
}

async fn origin_route(
    ConnectInfo(caller): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(q): Query<MediaQuery>,
    kind: OriginKind,
) -> axum::response::Response {
    if let Err(r) = enforce_localhost(&caller) {
        return r;
    }
    let Some(peer) = q.peer.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        return match daemon.origin_offers(kind).await {
            Ok(offers) => (
                StatusCode::OK,
                Json(serde_json::json!({ "offering": offers })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    };
    match daemon.origin_reach(peer, kind).await {
        Ok(reach) => (StatusCode::OK, Json(serde_json::json!(reach))).into_response(),
        Err(e @ MediaReachRefusal::UnknownMember(_)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        // Every other refusal is "the request is coherent and the mesh's
        // state is what says no" — 409, as `forget-member` reports it.
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
