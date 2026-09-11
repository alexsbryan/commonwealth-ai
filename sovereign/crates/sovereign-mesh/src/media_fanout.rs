// SPDX-License-Identifier: AGPL-3.0-or-later
//! Federated media, the catalogue half: one request to every member that
//! offers a library, answered as one attributed document.
//!
//! `svrn mesh media` (bare) says WHO offers; `svrn mesh media <peer>` reaches
//! ONE of them. A shim building a merged catalogue needs the third shape:
//! ask them all the same thing — `GET /Items?…`, whatever the origin speaks —
//! and get back a row per member saying what it answered, or why it was not
//! asked, in one round trip. That is `POST /v1/mesh/media/fanout`, and it is
//! `commonwealth_api::fanout` (the peer half of the knowledge fan-out,
//! extracted 2026-09-11) with a media origin at the far end of each row.
//!
//! **What it does not do.** No merge, no dedup, no item schema: item
//! semantics are the origin's, and a shim that speaks Jellyfin merges better
//! than this repository ever could. No streams either — a body is capped per
//! member and the row says `truncated`; a title is played through the
//! per-member URL the verb prints. Rows are collected and returned once, under
//! a per-member timeout, so one stalled relay costs its own row and nothing
//! else's.
//!
//! **Every member named is a row.** `peers` naming a member that offers no
//! origin, is offline, or is this node gets a `never_asked` row carrying the
//! same refusal `svrn mesh media <peer>` would print — never an absence in the
//! list (the cloud-peer flight's lesson, note 60d4d79b). With no `peers`, the
//! targets are exactly the members `svrn mesh media` lists.
//!
//! Since 2026-09-11 (cw-lift D1) the selection, the request validation and
//! the per-origin ask are `commonwealth-media`'s and only the route and the
//! daemon glue are here, so the inference daemon and the package-only rails
//! daemon fan out with one implementation (ARCH §10.6).
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Json};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;
use crate::media_reach::MediaReachRefusal;

/// The catalogue fan-out's own vocabulary, re-exported under its old path so
/// the CLI and the route keep naming it here.
pub use commonwealth_media::fanout::{
    ask_origin, select_targets, MediaAnswer, MediaFanoutRequest, MediaFanoutResponse,
    OriginRequest, Selected, DEFAULT_MAX_BODY_BYTES, DEFAULT_TIMEOUT_MS,
};

impl EmbeddedDaemon {
    /// Ask every selected member the same request through its own media
    /// bridge, concurrently, and return one row per member.
    pub async fn media_fanout(
        &self,
        req: MediaFanoutRequest,
    ) -> Result<MediaFanoutResponse, MediaReachRefusal> {
        let app_state = self.app_state().await.ok_or(MediaReachRefusal::NoMesh)?;
        let self_id = app_state.self_node_id();
        // Cloned out before any await: nothing here holds the mesh lock
        // across a dial.
        let roster = {
            let mesh = app_state.inner.mesh.read().await;
            commonwealth_media::roster_of(&mesh)
        };
        commonwealth_media::fanout::fanout(
            self_id,
            &roster,
            req,
            app_state.peer_transport(),
            app_state.inner.fanout_inflight.clone(),
        )
        .await
    }
}

/// `POST /v1/mesh/media/fanout` — loopback-only like every `/v1/mesh/*`
/// route. A malformed request is 400 with the reason; no mesh is 409.
pub async fn mesh_media_fanout(
    ConnectInfo(caller): ConnectInfo<std::net::SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<MediaFanoutRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&caller) {
        return r;
    }
    match daemon.media_fanout(req).await {
        Ok(doc) => (StatusCode::OK, Json(serde_json::json!(doc))).into_response(),
        Err(e @ MediaReachRefusal::BadRequest(_)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
