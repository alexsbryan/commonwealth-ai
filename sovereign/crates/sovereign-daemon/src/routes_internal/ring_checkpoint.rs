// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET /internal/ring/checkpoint/{ns}` — one ring's record, frozen: the
//! v1 checkpoint document (`docs/THE_LINK.md` §"The checkpoint, specified").
//!
//! One route, one document, nothing derived twice. `ops` are the journal
//! lines verbatim — each op re-serialises to exactly the bytes
//! `Oplog::append` wrote (`serde_json::to_string` is the journal's own
//! writer, and `serde_json::Value`'s canonical key order makes the round
//! trip stable) — `digest` is `commonwealth_rail::digest` over those same
//! ops, `roster` is the struct the append path admits under
//! ([`RingRail::roster`], the rail's ONE roster reader), `created_unix` is
//! now. A verifier trusts nothing here: it re-runs admit and recomputes the
//! digest, so the document is worth exactly what its signatures are worth.
//!
//! The read is the append path's own pair — `rail.roster(&journal)` plus ONE
//! `journal.read()` (`routes_rail.rs`) — so the roster and the ops cannot
//! come from two different answers to who holds this ring.
//!
//! Mounted on the internal router behind `internal_gate`: a CLI or the
//! desktop on loopback is admitted, a mesh peer has no business freezing a
//! copy of this node's record — it syncs by digest instead.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::state::AppState;

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// GET /internal/ring/checkpoint/{ns} — the namespace's record as one
/// self-describing document.
///
/// A namespace this node does not hold is REFUSED before anything is opened:
/// [`commonwealth_rail::RingRail::journal`] creates on first touch, so
/// asking it directly would materialise an empty ring for a typo and answer
/// it with a confident, empty document (ARCH §18.3 — absence is reported,
/// never defaulted).
pub async fn ring_checkpoint(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
) -> Response {
    let Some(rail) = state.ring_rail() else {
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            "this daemon has no ring storage installed, so there is no journal \
             to freeze — start it with a data directory",
        );
    };
    let held = match rail.namespaces() {
        Ok(names) => names,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if !held.iter().any(|name| name == &namespace) {
        return err(
            StatusCode::NOT_FOUND,
            format!("this node holds no ring named `{namespace}` — nothing to checkpoint"),
        );
    }
    let journal = match rail.journal(&namespace) {
        Ok(j) => j,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let roster = match rail.roster(&journal).await {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // ONE read, and the document is composed from exactly these bytes.
    let (ops, _skipped) = match journal.read() {
        Ok(pair) => pair,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let lines = ops
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>();
    let lines = match lines {
        Ok(l) => l,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("checkpoint: a journal line could not be carried verbatim: {e}"),
            )
        }
    };
    let actors = ops
        .iter()
        .map(|op| op.actor.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    tracing::info!(
        namespace = %namespace,
        acts = ops.len(),
        actors,
        "ring checkpoint: exported"
    );
    Json(serde_json::json!({
        "v": 1,
        "ns": namespace,
        "created_unix": commonwealth_core::clock::unix_now_secs(),
        "roster": roster,
        "digest": commonwealth_rail::digest(&ops),
        "ops": lines,
    }))
    .into_response()
}

#[cfg(test)]
#[path = "ring_checkpoint/tests.rs"]
mod tests;
