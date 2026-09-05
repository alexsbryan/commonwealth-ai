// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/ring/sync` — one anti-entropy exchange for one ring
//! namespace.
//!
//! # The shape, and why it is one route
//!
//! The caller sends what it holds (a per-actor contiguous high-water
//! [`Digest`]) and, optionally, ops it believes the responder lacks. The
//! responder ingests those, then answers with its own digest and as much of
//! what the caller's digest says it is missing as fits one budget. Two calls
//! converge both directions: the first learns the peer's digest, the second
//! delivers against it. Both are idempotent, so a dropped call costs one round
//! and never a duplicate entry.
//!
//! # Both directions are budgeted, and neither is capped
//!
//! `ops` on the way in and `ops` on the way out are each stopped at
//! [`RING_SYNC_OPS_BUDGET_BYTES`], and the sender repeats the exchange until
//! nothing moves. Nothing on the wire changed shape to make that work — the
//! exchange was always idempotent, so a partial one is safe.
//!
//! Before that, one exchange carried the whole selection and the receiver's
//! `DefaultBodyLimit` refused it at ~9,599 ops of the measured fixture. The
//! refusal was answered at the extractor, so this handler never ran: no gauge
//! fired, the sender filed the 413 as an unreachable peer, and the peer that
//! had been refused the journal reported zero ops, zero gaps and a COMPLETE
//! ring. A budget is the fix a bigger limit would only have postponed.
//!
//! # Nothing here validates an op, and that is the design
//!
//! Sibling route `/internal/app/state` validates nothing about an entry
//! either — the difference is that here it does not matter. An op carries its
//! author in an Ed25519 signature over a message that binds the namespace, so
//! a forged or replayed op does not become a balance: it becomes a
//! [`RailGap`](commonwealth_rail::RailGap) the next time anybody
//! folds. Checking here instead would put a second answer beside the fold's
//! (ARCH §10.6), and the fold's is the one that has to be right anyway,
//! because ops also arrive from disk.
//!
//! This port is reachable by any peer that can route to this host (see the
//! module header on `routes_internal`), so "who may write to my journal" is a
//! question the signature answers and the listener cannot.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_rail::{Digest, Op, SignedOp};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// The byte budget one exchange's `ops` array may fill, in either direction.
///
/// **ONE decider** (ARCH §10.6): derived from the receiver's body limit and
/// never re-typed, the same shape `MESH_STORE_PAYLOAD_WARN_BYTES` uses at
/// `sovereign-mesh/src/gossip.rs:108`. Half the limit, so the digest, the
/// namespace and the JSON scaffolding around the array have four megabytes of
/// headroom they will never need — and a peer running a build whose limit is
/// lower than ours still has room under it.
///
/// It is a BUDGET, not a cap. `ops_missing_from_within` hands back what fits
/// and says that more remains; the sender repeats the exchange
/// (`sovereign-mesh/src/ring_sync.rs::exchange`) until nothing does. Before
/// this existed the whole selection went in one body, and past
/// ~9,599 ops of the measured fixture the receiver answered 413 at the
/// extractor — so the handler never ran, no gauge fired, and the refused peer
/// reported a complete and empty ring.
pub const RING_SYNC_OPS_BUDGET_BYTES: usize = crate::server::MAX_REQUEST_BODY_BYTES / 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct RingSyncRequest {
    /// Which ring's journal. Named explicitly because this is peer-to-peer
    /// traffic and carries no grant — the grant scoping in `routes_rail` is
    /// about a deployed APP, which is a different principal from a peer node.
    pub namespace: String,
    /// What the caller holds. Empty means "I hold nothing", which asks for
    /// everything rather than defaulting to nothing.
    #[serde(default)]
    pub digest: Digest,
    /// Ops the caller believes this node lacks. Ingested as-signed.
    #[serde(default)]
    pub ops: Vec<Op<SignedOp>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RingSyncResponse {
    pub namespace: String,
    /// What THIS node holds, so the caller can compute what to send next.
    pub digest: Digest,
    /// Every op this node holds that the caller's digest says it lacks.
    pub ops: Vec<Op<SignedOp>>,
    /// How many of the caller's ops were new here. Zero is the steady state,
    /// not a failure.
    ///
    /// `#[serde(default)]` on the READ side only — this node always writes it.
    /// It preserves exactly the tolerance the sovereign-mesh client carried
    /// before it stopped declaring its own copy of this struct.
    #[serde(default)]
    pub ingested: usize,
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// The body arrives as raw [`Bytes`] rather than `Json<RingSyncRequest>` for
/// exactly one reason: **the gauge below has to read the direction that can
/// fail.** `DefaultBodyLimit` bounds the REQUEST; the response has no cap at
/// all. Measuring the deserialised struct back would be a second answer to
/// "how big was this" (ARCH §10.6) and would not be the number the extractor
/// compared against anyway.
pub async fn ring_sync(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    // Read before anything can fail, so a 400 still carries the size.
    let request_bytes = body.len();
    let req: RingSyncRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("malformed body: {e}")),
    };
    let Some(rail) = state.ring_rail() else {
        // A node with no ring storage cannot participate. Refusing is the
        // honest answer; a 200 with an empty digest would tell the peer this
        // node holds nothing, and it would stop offering ops (ARCH §18.3).
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            "this node has no ring storage installed",
        );
    };
    let journal = match rail.journal(&req.namespace) {
        Ok(l) => l,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let ingested = match journal.ingest_all(&req.ops) {
        Ok(n) => n,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // Budgeted in BOTH directions. The response was the unbounded half —
    // the pull direction converged at a size where the identical peer being
    // pushed to was refused — and an unbounded body is also an unbounded
    // allocation on a route any peer that can route here may call.
    let selection = journal.ops_missing_from_within(&req.digest, RING_SYNC_OPS_BUDGET_BYTES);
    let (digest, (ops, more_for_caller)) = match (journal.digest(), selection) {
        (Ok(d), Ok(o)) => (d, o),
        (Err(e), _) | (_, Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let body = RingSyncResponse {
        namespace: req.namespace.clone(),
        digest,
        ops,
        ingested,
    };
    let bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    tracing::debug!(
        namespace = %req.namespace,
        request_bytes,
        offered = req.ops.len(),
        ingested,
        sending = body.ops.len(),
        more_for_caller,
        payload_bytes = bytes.len(),
        "ring sync: exchange"
    );
    // **The gauge watches the request, because the request is the direction
    // with a limit.** It read `bytes.len()` — the response — until 2f, which
    // meant the rail's one instrument watched the half that cannot fail.
    //
    // A caller whose body reached the budget filled a whole chunk, so it has
    // more to send and will be back this round: that is the named trigger for
    // the checkpoint work the journal defers, and it is now also how an
    // UNBUDGETED sender (an older build, which puts its whole selection in
    // one body) becomes visible before the extractor refuses it.
    if request_bytes >= RING_SYNC_OPS_BUDGET_BYTES {
        tracing::warn!(
            namespace = %req.namespace,
            request_bytes,
            budget_bytes = RING_SYNC_OPS_BUDGET_BYTES,
            limit_bytes = crate::server::MAX_REQUEST_BODY_BYTES,
            offered = req.ops.len(),
            "ring sync: a caller filled its whole exchange budget — this ring \
             is carrying more history than one exchange holds, which is the \
             named trigger for journal checkpoints"
        );
    }
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}
