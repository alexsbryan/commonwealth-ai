// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code server's `/mcp/stats`. Its two MCP ports, [`CodeTools`] and
//! [`CodeCallLog`], moved with code's face to `sovereign_code::face`
//! (pb-code-daemon-exit); this path is their historical one.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use host_kit::locality::RequestLocality;
use sovereign_contracts::mcp_host::call_stats;
use sovereign_contracts::ToolRegistry;

#[allow(unused_imports)]
pub use sovereign_code::face::{CodeCallLog, CodeTools};

/// GET /mcp/stats — tool call counts since server start; `svrn serve
/// --background` probes it for readiness.
pub(crate) async fn mcp_stats(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Extension(tools): Extension<Arc<ToolRegistry>>,
) -> axum::response::Response {
    if !RequestLocality::of(&peer, &headers).is_local() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "local-only"})),
        )
            .into_response();
    }
    (StatusCode::OK, Json(call_stats(&tools))).into_response()
}
