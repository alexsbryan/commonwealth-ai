// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three loopback routes a shim author touches, and nothing else.
//!
//! `GET /v1/mesh/status` · `GET /v1/mesh/media[?peer=]` ·
//! `GET /v1/mesh/app[?peer=]` · `POST /v1/mesh/media/fanout`. Every answer is
//! `commonwealth_media`'s — the same functions the inference daemon's
//! `/v1/mesh/*` routes call, so a shim written against one daemon behaves the
//! same against the other (ARCH §10.6). This module is the HTTP shape and the
//! status mapping, and that is all it is.
//!
//! **Loopback IS the auth.** There is no bearer here, and the bind refuses
//! any address that is not loopback rather than serving an unauthenticated
//! API to a network. A URL this hands back is a bridge on this machine's own
//! `127.0.0.1` and is useless anywhere else in any case.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::capabilities::OriginKind;
use commonwealth_media::fanout::MediaFanoutRequest;
use commonwealth_media::MediaReachRefusal;
use serde::Deserialize;

use crate::{RailsDaemon, Refusal};

/// The one bind decision. A rails daemon has no non-loopback posture, so this
/// is a total function of the port rather than a configurable address.
pub fn bind_addr(port: u16) -> SocketAddr {
    ([127, 0, 0, 1], port).into()
}

pub fn router(daemon: Arc<RailsDaemon>) -> Router {
    Router::new()
        .route("/v1/mesh/status", get(status))
        .route("/v1/mesh/media", get(media))
        .route("/v1/mesh/app", get(app))
        .route("/v1/mesh/media/fanout", post(media_fanout))
        .with_state(daemon)
}

/// The bind guard, as a function so it has a failing input that can be
/// named without standing an iroh endpoint up. This API has no auth because
/// loopback IS the auth; anywhere else is publishing it.
pub fn check_loopback(listen: SocketAddr) -> Result<(), Refusal> {
    if listen.ip().is_loopback() {
        Ok(())
    } else {
        Err(Refusal::NotLoopback(listen))
    }
}

/// Bind and serve. Refuses a non-loopback address by name.
pub async fn serve(
    daemon: Arc<RailsDaemon>,
    listen: SocketAddr,
) -> Result<tokio::task::JoinHandle<()>, Refusal> {
    check_loopback(listen)?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| Refusal::Listen(listen, e))?;
    let app = router(daemon);
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(target: "rails", error = %e, "api: listener stopped");
        }
    }))
}

/// `GET /v1/mesh/status` — who I am, who is on the roster, who offers media.
pub async fn status(State(daemon): State<Arc<RailsDaemon>>) -> impl IntoResponse {
    let mesh = daemon.mesh.read().await;
    let members: Vec<serde_json::Value> = {
        let mut rows: Vec<_> = mesh
            .members
            .values()
            .filter(|m| m.removed_at.is_none())
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows.into_iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "node_id": m.node_id.to_string(),
                    "status": m.status,
                    // The catalogue's fact, read from the gossiped record
                    // rather than derived a second way here. `offers_media`
                    // keeps its name and meaning for the shims already
                    // parsing it; `offers` carries the whole set now that a
                    // node can publish more than one kind.
                    "offers_media": commonwealth_media::candidate_of(m).offers(OriginKind::Media),
                    "offers": commonwealth_media::candidate_of(m).origins,
                    "last_seen": m.last_seen,
                    "is_self": m.node_id == daemon.node.self_id,
                })
            })
            .collect()
    };
    let addr = daemon.node.endpoint.addr();
    Json(serde_json::json!({
        "self": {
            "node_id": daemon.node.self_id.to_string(),
            "name": daemon.node.config.name,
            "pubkey": hex::encode(daemon.node.pubkey().0),
            "dial": commonwealth_transport::iroh::format_dial_string(&addr),
            "media_origin": daemon.node.config.media.origin,
        },
        "mesh": { "id": mesh.id.to_string(), "name": mesh.name },
        "members": members,
        "fanout_inflight": daemon.gauge.load(Ordering::Relaxed),
        "internal_listener": daemon.internal_addr.to_string(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct MediaQuery {
    /// Member name or node-id prefix (≥4 chars). Absent: list who offers.
    #[serde(default)]
    pub peer: Option<String>,
}

/// `GET /v1/mesh/media` — the catalogue, or one member's loopback URL.
pub async fn media(state: State<Arc<RailsDaemon>>, q: Query<MediaQuery>) -> impl IntoResponse {
    origin(state, q, OriginKind::Media).await
}

/// `GET /v1/mesh/app` — the same two questions for PUBLISHED APPS.
///
/// A rails node can VIEW the house's apps here whether or not it publishes
/// any; the catalogue is the roster's gossip and the reach is a bridge over
/// `cwth/app/0`. What it cannot yet do is publish its OWN: that needs an
/// `[apps]` table in `rails.toml`, and both `Config` and `MediaSection` are
/// `#[serde(deny_unknown_fields)]`, so adding one makes an un-upgraded rails
/// daemon REFUSE TO BOOT on a config a new one wrote. That is a deployment
/// decision for the package daemon, not a side effect of this change, so it
/// is named here rather than taken quietly — the acceptor's `APP_ALPN` arm
/// lands with it.
pub async fn app(state: State<Arc<RailsDaemon>>, q: Query<MediaQuery>) -> impl IntoResponse {
    origin(state, q, OriginKind::App).await
}

async fn origin(
    State(daemon): State<Arc<RailsDaemon>>,
    Query(q): Query<MediaQuery>,
    kind: OriginKind,
) -> axum::response::Response {
    let roster = daemon.roster().await;
    let paths = daemon.paths().await;
    let self_id = daemon.node.self_id;
    let Some(peer) = q.peer.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        let offering = commonwealth_media::offers(self_id, &roster, &paths, kind);
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "offering": offering })),
        )
            .into_response();
    };
    match commonwealth_media::reach(self_id, &roster, peer, &daemon.transport, &paths, kind).await {
        Ok(reach) => (StatusCode::OK, Json(serde_json::json!(reach))).into_response(),
        // A name nobody has is the one refusal that is about the REQUEST.
        Err(e @ MediaReachRefusal::UnknownMember(_)) => refusal(StatusCode::NOT_FOUND, e),
        // Everything else is coherent and the mesh's state is what says no.
        Err(e) => refusal(StatusCode::CONFLICT, e),
    }
}

/// `POST /v1/mesh/media/fanout` — the same request to every offering member.
pub async fn media_fanout(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(req): Json<MediaFanoutRequest>,
) -> impl IntoResponse {
    let roster = daemon.roster().await;
    match commonwealth_media::fanout::fanout(
        daemon.node.self_id,
        &roster,
        req,
        daemon.transport.clone(),
        daemon.gauge.clone(),
    )
    .await
    {
        Ok(doc) => (StatusCode::OK, Json(serde_json::json!(doc))).into_response(),
        Err(e @ MediaReachRefusal::BadRequest(_)) => refusal(StatusCode::BAD_REQUEST, e),
        Err(e) => refusal(StatusCode::CONFLICT, e),
    }
}

/// One refusal shape for all three routes, so a shim parses `error` once.
fn refusal(code: StatusCode, e: MediaReachRefusal) -> axum::response::Response {
    tracing::info!(target: "rails", status = code.as_u16(), error = %e, "api: refused");
    (code, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bind_is_loopback_whatever_the_port() {
        assert!(bind_addr(9747).ip().is_loopback());
        assert_eq!(bind_addr(9747).port(), 9747);
    }

    /// **The failing input.** This API has no auth because loopback IS the
    /// auth; serving it on `0.0.0.0` would publish an unauthenticated route
    /// that mints bridges into other people's media servers. The refusal
    /// happens before the bind, so there is no window in which it is up.
    #[test]
    fn a_non_loopback_listen_address_is_refused_before_it_binds() {
        for public in ["0.0.0.0:9747", "192.168.1.8:9747", "[::]:9747"] {
            let err = check_loopback(public.parse().unwrap())
                .expect_err("a non-loopback address must be refused");
            assert!(matches!(err, Refusal::NotLoopback(_)), "{err}");
            assert!(err.to_string().contains("loopback IS the auth"), "{err}");
        }
        for local in ["127.0.0.1:9747", "[::1]:9747", "127.1.2.3:9747"] {
            check_loopback(local.parse().unwrap()).expect(local);
        }
    }
}
