// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared error-response builders for the sovereign-mesh HTTP surfaces.
//!
//! Why this exists: pre-consolidation, every route file (`admin_http`,
//! `mesh_http`, `corpus_watch_http`, `reading_http`, `landscape_digest_http`,
//! `project_http`, `mcp_router`) invented its own error-mapping
//! convention — inline `(StatusCode, Json(json!({"error": ...})))` tuples,
//! private `not_found` / `service_unavailable` / `error` helpers with
//! mutually-incompatible signatures, or JSON-RPC envelope structs.
//! Different shapes for the same outcome.
//!
//! These builders give one canonical `{"error": "..."}` body for the
//! common case. JSON-RPC framing in `mcp_router` keeps its own
//! `JsonRpcError` envelope — that's a protocol contract, not the same
//! concern.
//!
//! Migration status: introduced alongside the `enforce_localhost`
//! consolidation (2026-05-13). Per-file helpers in `corpus_watch_http`
//! and `reading_http` haven't been migrated yet (separate §14.1
//! concern). The `#[allow(dead_code)]` below disappears as each file
//! adopts these helpers.
#![allow(dead_code)]

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
