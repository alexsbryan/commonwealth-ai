// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/ring/live` — take one ephemeral payload from a peer.
//!
//! The receiving half of the live lane. The payload goes into the bounded
//! in-memory buffer on `AppState` and nowhere else: no store, no journal, no
//! disk. See [`crate::routes_rail_live`] for why the lane exists and why it
//! has no row in `REPLICATION_SENDERS`.
//!
//! **Nothing here validates the payload, and unlike `ring_sync`'s that is not
//! because a signature answers it later.** A live payload has no signature
//! and is never folded — it renders a cursor for a few hundred milliseconds
//! and is gone. So what this route is actually saying is: any peer that can
//! route to this host (see the `routes_internal` module header) can make a
//! cursor appear on this daemon's pages. That is the cost of the lane, and it
//! is bounded to `LIVE_PAYLOAD_MAX_BYTES` × `LIVE_BUFFER_CAPACITY` of memory
//! that the next drain frees. Attribution is NOT bounded here and is not
//! meant to be: a NAME on screen comes from a rail act's signer through the
//! roster, never from anything on this route
//! (`quality/campaigns/ring-doc.toml` bar `ra-doc-attribution-from-signer`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::routes_rail_live::LIVE_PAYLOAD_MAX_BYTES;
use crate::state::AppState;

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// POST /internal/ring/live — buffer one peer's ephemeral payload.
///
/// **An oversize payload is REFUSED, never truncated and never dropped
/// quietly** (ARCH §18.3). The size ceiling read here is the same constant
/// the sending half compares against, so a payload this daemon refuses is one
/// no peer of ours would have sent — and a 413 tells the sender which it was.
/// The silent-failure-above-4096 behaviour is precisely what
/// `ring-apps-shelf.md` §gossip records as the defect we would inherit from
/// iroh-gossip's default.
pub async fn ring_live(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    if body.len() > LIVE_PAYLOAD_MAX_BYTES {
        tracing::warn!(
            bytes = body.len(),
            max = LIVE_PAYLOAD_MAX_BYTES,
            "internal ring live: refused an oversize payload"
        );
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "live payload is {} bytes; this lane carries at most {}",
                body.len(),
                LIVE_PAYLOAD_MAX_BYTES
            ),
        );
    }
    let payload = match std::str::from_utf8(&body) {
        Ok(s) => s.to_string(),
        Err(e) => {
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("live payload is not UTF-8 ({e}) — this lane carries text"),
            )
        }
    };
    state.rail_live_buffer().push(payload);
    tracing::debug!(bytes = body.len(), "internal ring live: buffered");
    Json(serde_json::json!({ "buffered": body.len() })).into_response()
}
