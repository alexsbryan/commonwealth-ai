// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring rail's **live lane** — delivery, not record.
//!
//! `POST /v1/rail/live` fans one small payload out to every online peer
//! right now, each reached through cw-rails' reach door to that peer's
//! cw-rails `/internal/ring/live`; `GET /v1/rail/live` drains what arrived,
//! from the buffer the local cw-rails holds (pb-mesh-exit-transport). Nothing
//! here reaches a store, a journal or a disk, and that is the whole point:
//! presence is the case the durable lane provably cannot serve. y-protocols'
//! awareness `outdatedTimeout` is 30 s and the ring round's cadence is 60 s,
//! so a cursor carried by the journal would have faded before it arrived
//! (`quality/campaigns/ring-apps-shelf.md` §gossip).
//!
//! **The payload is opaque TEXT and this module never looks inside it.** The
//! page already base64s its Yjs bytes for a rail act (`cmnwlth/apps/ring-doc/
//! adapter.js`), so the live lane takes the same spelling rather than minting
//! a second one — and a daemon that decoded it would be claiming to know
//! what an app's presence means. Text also means the drain can hand the
//! bytes back verbatim in JSON with no encoder anywhere in this crate.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::client_auth::Guest;
use crate::routes_rail::{namespace_for, RailQuery};
use crate::state::AppState;

/// The largest live payload, in wire bytes.
///
/// Read by the client route that sends; the receiver is a peer's cw-rails
/// (commonwealth-rails rail.rs `LIVE_PAYLOAD_MAX_BYTES`, the same number)
/// since pb-mesh-exit-transport. The number is iroh-gossip's own
/// default `max_message_size`, which the shelf records as failing SILENTLY
/// above it (`ring-apps-shelf.md` §gossip, issue #131) — the failure this
/// lane refuses out loud instead.
pub const LIVE_PAYLOAD_MAX_BYTES: usize = 4096;

/// How long one peer has to take a live payload.
///
/// Short on purpose: a cursor that arrives late is worth nothing, and this
/// fan-out runs inside a client request the page makes every 100 ms.
const LIVE_PUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Moved to `sovereign_peer_wire` (phase-b pb-rails-parity): cw-rails'
/// `/internal/ring/live` reads the same body. Re-exported at its historical path.
pub use sovereign_peer_wire::LiveEnvelope;

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// What happened when one peer was offered a payload.
#[derive(Debug, serde::Serialize)]
struct PeerDelivery {
    node: String,
    name: Option<String>,
    delivered: bool,
    /// Why not, in the words of whatever refused. Present iff `delivered` is
    /// false — the page shows a live lane that is half up rather than
    /// believing a 200 meant everyone saw it.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// POST `bytes` to every online peer's `/internal/ring/live`, concurrently.
///
/// **The fan-out is `forward_to_peers`'s, one file over**
/// (`routes_internal/pipeline_pause.rs:302`): read `inner.mesh`, drop self
/// and anything not `Online`, resolve each peer's endpoints through the
/// `PeerTransport` seam, and try them in order until one answers. Direct
/// fan-out rather than iroh-gossip because at ring scale every pair already
/// holds an authenticated connection (`ring-apps-shelf.md` §gossip, "default
/// impl").
async fn push_ephemeral(state: &AppState, namespace: &str, payload: &str) -> Vec<PeerDelivery> {
    let self_id = state.inner.fabric.identity.current();
    let peers: Vec<_> = state
        .membership()
        .members()
        .await
        .into_iter()
        .filter(|m| m.node_id != self_id && m.status == crate::types::MemberStatus::Online)
        .collect();

    if peers.is_empty() {
        return Vec::new();
    }

    let client = match reqwest::Client::builder()
        .timeout(LIVE_PUSH_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "rail live: reqwest build failed");
            return peers
                .into_iter()
                .map(|m| PeerDelivery {
                    node: hex::encode(m.node_id.as_bytes()),
                    name: Some(m.name.clone()),
                    delivered: false,
                    error: Some(format!("local reqwest build failed: {e}")),
                })
                .collect();
        }
    };

    let envelope = serde_json::to_string(&LiveEnvelope {
        namespace: namespace.to_string(),
        payload: payload.to_string(),
    })
    .expect("a struct of two Strings always serializes");
    let transport = state.peer_transport();
    let mut handles = Vec::with_capacity(peers.len());
    for peer in peers {
        let client = client.clone();
        let envelope = envelope.clone();
        let node = hex::encode(peer.node_id.as_bytes());
        let name = Some(peer.name.clone());
        let endpoints = transport
            .endpoints(&peer.dial, mesh_reach::TrafficClass::ControlPlane)
            .await;
        handles.push(tokio::spawn(async move {
            offer_peer(&client, &envelope, node, name, &endpoints).await
        }));
    }

    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(result) => out.push(result),
            Err(join_err) => {
                tracing::warn!(error = %join_err, "rail live: peer task panicked");
            }
        }
    }
    out
}

/// Try each transport-resolved endpoint for one peer; the first that answers
/// wins. Candidate ordering is the `PeerTransport` seam's, so live traffic
/// routes the way every other peer call on this daemon does.
async fn offer_peer(
    client: &reqwest::Client,
    envelope: &str,
    node: String,
    name: Option<String>,
    endpoints: &[mesh_reach::PeerEndpoint],
) -> PeerDelivery {
    let mut last_error = "no addresses advertised by peer".to_string();
    for ep in endpoints {
        let url = format!("{}/internal/ring/live", ep.base_url);
        let request = client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(envelope.to_string());
        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                return PeerDelivery {
                    node,
                    name,
                    delivered: true,
                    error: None,
                };
            }
            Ok(resp) if resp.status() == StatusCode::NOT_FOUND => {
                last_error = format!(
                    "{url}: 404 — peer is running an older daemon without \
                     /internal/ring/live; rebuild + restart it to see cursors"
                );
                // Terminal for this peer: no other address routes differently.
                break;
            }
            Ok(resp) => {
                last_error = format!("{url}: {}", resp.status());
                continue;
            }
            Err(e) => {
                last_error = format!("{url}: {e}");
                continue;
            }
        }
    }
    tracing::debug!(
        node,
        endpoints = endpoints.len(),
        error = %last_error,
        "rail live: peer not reached"
    );
    PeerDelivery {
        node,
        name,
        delivered: false,
        error: Some(last_error),
    }
}

/// POST /v1/rail/live — send one ephemeral payload to every online peer.
///
/// The body is the payload, verbatim and unread. The namespace is the
/// grant's, resolved by `routes_rail::namespace_for` exactly as append and
/// log resolve theirs, and travels in the envelope to peers. Nothing is
/// written anywhere.
pub async fn live_push(
    State(state): State<AppState>,
    guest: Option<axum::Extension<Guest>>,
    Query(q): Query<RailQuery>,
    body: axum::body::Bytes,
) -> Response {
    let namespace = match namespace_for(
        &state.guest_pages(),
        guest.as_ref().map(|e| &e.0),
        q.namespace.as_deref(),
    ) {
        Ok(ns) => ns,
        Err(refusal) => return refusal,
    };
    if body.len() > LIVE_PAYLOAD_MAX_BYTES {
        tracing::debug!(
            namespace,
            bytes = body.len(),
            "rail live: refused an oversize payload"
        );
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "live payload is {} bytes; the lane carries at most {} — \
                 send presence, not state",
                body.len(),
                LIVE_PAYLOAD_MAX_BYTES
            ),
        );
    }
    let payload = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(namespace, error = %e, "rail live: refused a non-UTF-8 payload");
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "live payload is not UTF-8 ({e}) — the lane carries text \
                     (base64 your bytes, as a rail act does)"
                ),
            );
        }
    };
    let peers = push_ephemeral(&state, &namespace, payload).await;
    let delivered = peers.iter().filter(|p| p.delivered).count();
    tracing::debug!(
        namespace,
        bytes = body.len(),
        peers = peers.len(),
        delivered,
        "rail live: fanned out one payload"
    );
    Json(serde_json::json!({
        "bytes": body.len(),
        "peers": peers,
        "delivered": delivered,
    }))
    .into_response()
}

/// GET /v1/rail/live — drain everything peers have pushed to the caller's
/// namespace since the last call.
///
/// A drain, not a read: two pages polling one daemon would each see half the
/// payloads, which is why the page that owns a document is the only caller.
/// The buffer is cw-rails' (its `/internal/ring/live` receives every peer's
/// push, pb-mesh-exit-transport), so after the grant check this is a forward
/// to its `GET /v1/rail/live`, `{payloads, dropped}` verbatim; a cw-rails that
/// does not answer is a 503 naming it, never an empty drain.
pub async fn live_drain(
    State(state): State<AppState>,
    guest: Option<axum::Extension<Guest>>,
    Query(q): Query<RailQuery>,
) -> Response {
    let namespace = match namespace_for(
        &state.guest_pages(),
        guest.as_ref().map(|e| &e.0),
        q.namespace.as_deref(),
    ) {
        Ok(ns) => ns,
        Err(refusal) => return refusal,
    };
    match crate::rails_client::live_drain(&state.inner.node.rails_base, &namespace).await {
        Ok(body) => Json(body).into_response(),
        Err(e) => {
            tracing::warn!(namespace, error = %e, "rail live: cw-rails did not answer the drain");
            err(StatusCode::SERVICE_UNAVAILABLE, e.to_string())
        }
    }
}
