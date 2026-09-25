// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET /internal/ring/checkpoint/{ns}` — one ring's record, frozen: the
//! v1 checkpoint document (`docs/THE_LINK.md` §"The checkpoint, specified").
//!
//! One route, one document, nothing derived twice — the document itself is
//! composed by `sovereign_mesh::ring_checkpoint::checkpoint_document`, the one
//! composer the verifier's tests build with too. `ops` are the journal
//! lines verbatim — each op re-serialises to exactly the bytes
//! `Oplog::append` wrote (`serde_json::to_string` is the journal's own
//! writer, and `serde_json::Value`'s canonical key order makes the round
//! trip stable) — `digest` is `commonwealth_rail_core::digest` over those
//! same ops, `roster` is the struct the append path admits under (the port's
//! `roster`, the rail's ONE roster reader), `created_unix` is now. A verifier
//! trusts nothing here: it re-runs admit and recomputes the digest, so the
//! document is worth exactly what its signatures are worth.
//!
//! The read goes through the ring rail's PORT (`sovereign_mesh::rail_port`),
//! the same object the append route in `routes_rail.rs` writes through — the
//! daemon holds no journal of its own since fp-54, so the roster and the ops
//! come from the one place that admits appends, and the ops are ONE
//! `journal_read`.
//!
//! Mounted on the internal router behind `internal_gate`: a CLI or the
//! desktop on loopback is admitted, a mesh peer has no business freezing a
//! copy of this node's record — it syncs by digest instead.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commonwealth_rail_core::RailError;

use crate::state::AppState;

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// GET /internal/ring/checkpoint/{ns} — the namespace's record as one
/// self-describing document.
///
/// A namespace this node does not hold is REFUSED before anything is opened:
/// the local rail creates a journal on first touch, so asking it directly
/// would materialise an empty ring for a typo and answer it with a confident,
/// empty document (ARCH principle 6 — absence is reported, never defaulted).
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
    let held = match rail.namespaces().await {
        Ok(names) => names,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if !held.iter().any(|name| name == &namespace) {
        tracing::debug!(namespace = %namespace, "ring checkpoint: refused a ring this node does not hold");
        return err(
            StatusCode::NOT_FOUND,
            format!("this node holds no ring named `{namespace}` — nothing to checkpoint"),
        );
    }
    let roster = match rail.roster(&namespace).await {
        Ok(r) => r,
        Err(e @ RailError::BadNamespace(_)) => return err(StatusCode::BAD_REQUEST, e.to_string()),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    // ONE read, and the document is composed from exactly these ops.
    let ops = match rail.journal_read(&namespace).await {
        Ok(ops) => ops,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let actors = ops
        .iter()
        .map(|op| op.actor.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let document = match sovereign_mesh::ring_checkpoint::checkpoint_document(
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
    tracing::info!(
        namespace = %namespace,
        acts = ops.len(),
        actors,
        "ring checkpoint: exported"
    );
    Json(document).into_response()
}

#[cfg(test)]
#[path = "ring_checkpoint/tests.rs"]
mod tests;
