// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's ONE JSON error envelope, and the absence a handler returns
//! with `?`.
//!
//! Every route family in this crate answers a failure as
//! `{"error": "<message>"}` at some status. Before this file was adopted each
//! one built that body itself: thirteen copies of `struct ErrorBody`, sixteen
//! helper signatures spelling the same four statuses, and 83 four-line
//! `match store_for(&daemon) { Ok(s) => s, Err(resp) => return resp }` blocks
//! standing in for a `?`. One envelope with thirteen constructors is the
//! §10.6 smell: the copies cannot diverge loudly, only quietly.
//!
//! [`Absence`] is the half that made `?` possible — an "I cannot serve this,
//! here is the status and why" value, `IntoResponse`, so an accessor answers
//! `Result<T, Absence>` and a handler answers `Result<Response, Absence>`.
//! The shape is `governance_http`'s `GovError`, widened to the whole crate.
//!
//! JSON-RPC framing in `mcp_router` keeps its own `JsonRpcError` envelope —
//! that is a protocol contract, not the same concern.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Build a `{"error": "<message>"}` JSON response at the given status.
/// Use this from any route handler that needs to short-circuit with a
/// human-readable error.
pub(crate) fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    let body = serde_json::json!({ "error": message.into() });
    (status, Json(body)).into_response()
}

/// 404 with `{"error": "<message>"}`.
pub(crate) fn not_found(message: impl Into<String>) -> Response {
    json_error(StatusCode::NOT_FOUND, message)
}

/// 500 with `{"error": "<message>"}`. Reserve for unexpected internal
/// failures — see `service_unavailable` for "the engine isn't ready
/// yet" cases.
pub(crate) fn internal_error(message: impl Into<String>) -> Response {
    json_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

/// 503 with `{"error": "<message>"}`. Use when a dependency the route
/// needs (corpus engine, mesh state, …) hasn't been wired yet —
/// distinct from `internal_error`, which signals an unexpected fault.
pub(crate) fn service_unavailable(message: impl Into<String>) -> Response {
    json_error(StatusCode::SERVICE_UNAVAILABLE, message)
}

/// 400 with `{"error": "<message>"}`.
pub(crate) fn bad_request(message: impl Into<String>) -> Response {
    json_error(StatusCode::BAD_REQUEST, message)
}

/// 501 with `{"error": "<message>"}`. "This host cannot judge that" —
/// `recipe_project_http`'s workflow TOML is the shipped case. Distinct from
/// `service_unavailable`: retrying later will not help.
pub(crate) fn not_implemented(message: impl Into<String>) -> Response {
    json_error(StatusCode::NOT_IMPLEMENTED, message)
}

/// **Why a handler cannot serve this request** — the status and the sentence,
/// carried as a value so `?` can return it.
///
/// Every accessor in this crate (`store_for`, `engine_for`, `manager_or_503`,
/// `index_path`, …) answers `Result<T, Absence>`, so a handler reads
/// `let store = store_for(&daemon)?;` instead of restating the four-line
/// match. The bytes are [`json_error`]'s, so adopting it changed no response.
///
/// Deliberately not an enum of reasons: the reason is the MESSAGE, which is
/// per-site prose, and a closed set of causes would have to be re-opened for
/// every new one. What is closed is the STATUS, and each constructor names it.
#[derive(Debug, Clone)]
pub(crate) struct Absence {
    status: StatusCode,
    message: String,
}

impl Absence {
    /// The dependency this route needs was never wired on this daemon (503).
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::at(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    /// No such row, corpus, atom or id — the caller's list is stale (404).
    pub(crate) fn missing(message: impl Into<String>) -> Self {
        Self::at(StatusCode::NOT_FOUND, message)
    }

    /// The request is malformed on its own terms (400).
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::at(StatusCode::BAD_REQUEST, message)
    }

    /// Ours: a read that failed, an append that would not land (500).
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::at(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    /// This host cannot answer that question at all (501).
    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::at(StatusCode::NOT_IMPLEMENTED, message)
    }

    /// Any other status, for a site that already decided one.
    pub(crate) fn at(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for Absence {
    fn into_response(self) -> Response {
        json_error(self.status, self.message)
    }
}
