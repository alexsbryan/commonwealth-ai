// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/ring/sync` — one anti-entropy exchange for one ring
//! namespace.
//!
//! # The shape, and why it is one route
//!
//! The caller sends what it holds (a per-actor contiguous high-water
//! [`Digest`]) and, optionally, ops it believes the responder lacks. The
//! responder ingests those, then answers with its own digest and every op the
//! caller's digest says it is missing. Two calls converge both directions:
//! the first learns the peer's digest, the second delivers against it. Both
//! are idempotent, so a dropped call costs one round and never a duplicate
//! entry.
//!
//! # Nothing here validates an op, and that is the design
//!
//! Sibling route `/internal/app/state` validates nothing about an entry
//! either — the difference is that here it does not matter. An op carries its
//! author in an Ed25519 signature over a message that binds the namespace, so
//! a forged or replayed op does not become a balance: it becomes a
//! [`RailGap`](commonwealth_knowledge::RailGap) the next time anybody
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
use commonwealth_knowledge::rail::{Digest, SignedOp};
use corpus_engine::oplog::Op;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Past this, a single exchange is carrying enough history that the
/// checkpoint work the journal deferred has become due. Half the receiver's
/// body limit, matching the gauge `gossip.rs` already keeps on the mesh-store
/// snapshot — a rail, not a cap: the exchange still goes through.
const RING_PAYLOAD_WARN_BYTES: usize = crate::server::MAX_REQUEST_BODY_BYTES / 2;

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

pub async fn ring_sync(
    State(state): State<AppState>,
    Json(req): Json<RingSyncRequest>,
) -> Response {
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
    let (digest, ops) = match (journal.digest(), journal.ops_missing_from(&req.digest)) {
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
        offered = req.ops.len(),
        ingested,
        sending = body.ops.len(),
        payload_bytes = bytes.len(),
        "ring sync: exchange"
    );
    if bytes.len() >= RING_PAYLOAD_WARN_BYTES {
        tracing::warn!(
            namespace = %req.namespace,
            payload_bytes = bytes.len(),
            warn_at_bytes = RING_PAYLOAD_WARN_BYTES,
            limit_bytes = crate::server::MAX_REQUEST_BODY_BYTES,
            "ring sync: one exchange is past half the receiver's body limit — \
             this is the named trigger for journal checkpoints, and past the \
             limit peers stop converging"
        );
    }
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}
