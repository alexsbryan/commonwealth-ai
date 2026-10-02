// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring's peer routes on `cwth/http/0` (phase-b pb-rails-parity):
//! `POST /internal/ring/sync`, `POST /internal/ring/live` and
//! `GET /internal/ring/checkpoint/{ns}` — what the daemon's
//! `routes_internal::{ring_sync, ring_live, ring_checkpoint}` answer today, so
//! the flip turns nothing off.
//!
//! They sit on the internal listener beside gossip and join, under the
//! standing `/internal/ring` prefix the endpoint registers for MEMBERS
//! ([`crate::origins::stand_own`]): a dialer the roster does not name never
//! reaches them. What a member may then read is each ring's own question —
//! the sync route asks the ring's roster about the verified key the acceptor
//! stamped (`X-Mesh-Pubkey`), through [`crate::ring_sync::roster_names`], the
//! one test the sender also applies. A caller with no stamp reached this
//! loopback port without the acceptor in front, so it is a process on this
//! machine, and is served as the daemon serves one.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use commonwealth_core::ids::NodePubkey;
use commonwealth_media::origins::OriginRegistry;
use commonwealth_rail::{RailError, RingRail};
use host_kit::shell::RouteBundle;
use sovereign_peer_wire::{
    LiveEnvelope, RingSyncRequest, MAX_REQUEST_BODY_BYTES, RING_SYNC_OPS_BUDGET_BYTES,
};

use crate::rail::{err, LiveBuffer, LIVE_PAYLOAD_MAX_BYTES};
use crate::ring_sync::{answer, roster_names, RingSyncJournal};

/// The header the acceptor stamps with the dialer's verified key
/// (`kernel_types::member::verified_headers`); a client-typed one is stripped
/// before the forward.
const PUBKEY_HEADER: &str = "x-mesh-pubkey";

/// What the ring routes read: the journals, the live lane's buffer and the
/// origin registry, whose registrations name the live namespaces.
#[derive(Clone)]
pub struct RingInbound {
    pub rail: Arc<RingRail>,
    pub live: Arc<LiveBuffer>,
    pub origins: OriginRegistry,
}

/// The three routes, bundled for the internal listener. The sync route takes
/// the peer-wire body limit, because its exchange budget is half of it.
pub fn router(state: RingInbound) -> RouteBundle {
    RouteBundle::new("internal-ring")
        .route(
            "/internal/ring/sync",
            post(ring_sync).layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES)),
        )
        .route("/internal/ring/live", post(ring_live))
        .route("/internal/ring/checkpoint/{ns}", get(ring_checkpoint))
        .with_state(state)
}

/// Why this asker may not have this namespace, or `None` to serve it.
async fn roster_refusal(rail: &RingRail, headers: &HeaderMap, namespace: &str) -> Option<String> {
    let Some(stamped) = headers.get(PUBKEY_HEADER) else {
        tracing::debug!(
            target: "rails",
            namespace,
            "ring sync: no verified key stamped — a process on this machine, served"
        );
        return None;
    };
    let key = match stamped
        .to_str()
        .ok()
        .and_then(|s| hex::decode(s.trim()).ok())
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    {
        Some(bytes) => NodePubkey(bytes),
        None => {
            tracing::warn!(target: "rails", namespace, "ring sync: refused — the stamped key is not one");
            return Some(format!(
                "{namespace} is served to its roster, and the asker's key could not be read"
            ));
        }
    };
    let roster = match RingSyncJournal::roster(rail, namespace).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "rails", namespace, error = %e,
                "ring sync: refused — this namespace's roster is unreadable, so nobody can be shown to be on it");
            return Some(format!("{namespace}'s roster is unreadable on this node"));
        }
    };
    if roster_names(&roster, Some(key)) {
        tracing::debug!(target: "rails", namespace, asker = %key, "ring sync: the roster names the asker");
        return None;
    }
    tracing::warn!(target: "rails", namespace, asker = %key,
        "ring sync: refused — this ring's roster does not name the asker");
    Some(format!("{key} is not on {namespace}'s roster"))
}

/// `POST /internal/ring/sync` — one anti-entropy exchange for one namespace.
/// The body is raw bytes so the gauge reads the direction with a limit.
pub async fn ring_sync(
    State(state): State<RingInbound>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let request_bytes = body.len();
    let req: RingSyncRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("malformed body: {e}")),
    };
    if let Some(refusal) = roster_refusal(&state.rail, &headers, &req.namespace).await {
        return err(StatusCode::FORBIDDEN, refusal);
    }
    let (response, more_for_caller) = match answer(state.rail.as_ref(), &req).await {
        Ok(pair) => pair,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let bytes = match serde_json::to_vec(&response) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    tracing::debug!(
        target: "rails",
        namespace = %req.namespace,
        request_bytes,
        offered = req.ops.len(),
        ingested = response.ingested,
        sending = response.ops.len(),
        more_for_caller,
        payload_bytes = bytes.len(),
        "ring sync: exchange"
    );
    if request_bytes >= RING_SYNC_OPS_BUDGET_BYTES {
        tracing::warn!(
            target: "rails",
            namespace = %req.namespace,
            request_bytes,
            budget_bytes = RING_SYNC_OPS_BUDGET_BYTES,
            limit_bytes = MAX_REQUEST_BODY_BYTES,
            "ring sync: a caller filled its whole exchange budget — this ring is \
             carrying more history than one exchange holds"
        );
    }
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}

/// `POST /internal/ring/live` — buffer one peer's ephemeral payload under the
/// namespace its envelope names. A namespace no live registration on this
/// endpoint holds is REFUSED: nobody here could drain it (the daemon's rule,
/// with a registration where the daemon reads a grant). An oversize payload
/// is refused against the one cap the push door keeps.
pub async fn ring_live(State(state): State<RingInbound>, body: axum::body::Bytes) -> Response {
    let LiveEnvelope { namespace, payload } = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(target: "rails", error = %e, "ring live: refused a malformed envelope");
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("live envelope is not {{\"namespace\", \"payload\"}} JSON ({e})"),
            );
        }
    };
    if payload.len() > LIVE_PAYLOAD_MAX_BYTES {
        tracing::warn!(target: "rails", namespace, bytes = payload.len(),
            max = LIVE_PAYLOAD_MAX_BYTES, "ring live: refused an oversize payload");
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "live payload is {} bytes; this lane carries at most {}",
                payload.len(),
                LIVE_PAYLOAD_MAX_BYTES
            ),
        );
    }
    if !state.origins.namespaces().iter().any(|n| n == &namespace) {
        tracing::warn!(target: "rails", namespace,
            "ring live: refused a payload for a namespace no live registration here holds");
        return err(
            StatusCode::FORBIDDEN,
            format!(
                "no live registration on this endpoint holds namespace '{namespace}' — \
                 nobody here could drain it, so it is not buffered"
            ),
        );
    }
    let bytes = payload.len();
    state.live.push(&namespace, payload);
    tracing::debug!(target: "rails", namespace, bytes, "ring live: buffered");
    Json(serde_json::json!({ "buffered": bytes })).into_response()
}

/// `GET /internal/ring/checkpoint/{ns}` — the namespace's record as one
/// self-describing document, composed by the one composer
/// ([`commonwealth_rail::ring_checkpoint::checkpoint_document`]). A namespace
/// this node does not hold is REFUSED before anything is opened: the rail
/// creates a journal on first touch.
pub async fn ring_checkpoint(
    State(state): State<RingInbound>,
    Path(namespace): Path<String>,
) -> Response {
    let held = match state.rail.namespaces() {
        Ok(names) => names,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if !held.iter().any(|name| name == &namespace) {
        tracing::debug!(target: "rails", namespace = %namespace, "ring checkpoint: refused a ring this node does not hold");
        return err(
            StatusCode::NOT_FOUND,
            format!("this node holds no ring named `{namespace}` — nothing to checkpoint"),
        );
    }
    let roster = match RingSyncJournal::roster(state.rail.as_ref(), &namespace).await {
        Ok(r) => r,
        Err(e @ RailError::BadNamespace(_)) => return err(StatusCode::BAD_REQUEST, e.to_string()),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let ops = match state.rail.journal(&namespace).and_then(|j| j.read()) {
        Ok((ops, _skipped)) => ops,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let actors = ops
        .iter()
        .map(|op| op.actor.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let document = match commonwealth_rail::ring_checkpoint::checkpoint_document(
        &namespace,
        &roster,
        &ops,
        commonwealth_core::clock::unix_now_secs(),
    ) {
        Ok(d) => d,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("checkpoint: a journal line could not be carried verbatim: {e}"),
            )
        }
    };
    tracing::info!(target: "rails", namespace = %namespace, acts = ops.len(), actors,
        "ring checkpoint: exported");
    Json(document).into_response()
}

#[cfg(test)]
mod tests;
