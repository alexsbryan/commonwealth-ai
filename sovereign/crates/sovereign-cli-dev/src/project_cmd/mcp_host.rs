// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code server's two MCP ports (phase-b pb-code-server): [`CodeTools`],
//! the host kit's tool host over code's registry, and [`CodeCallLog`], its
//! call log over the NoteStore that `svrn reflect` and the pattern matcher
//! read. The ToolRegistry half of a call is sovereign-contracts' `mcp_host`,
//! the one the daemon runs too; what code exposes is code's own list,
//! `sovereign_code::mcp_surface` (pb-code-freshness).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use corpus_engine_notes::mining::patterns::ToolPatternMatcher;
use corpus_engine_notes::NoteStore;
use host_kit::mcp::{McpCallLog, McpRequestContext, McpToolHost, ToolOutcome};
use serde_json::Value;
use sovereign_contracts::mcp_host::{
    agent_session_token, call_log_tag, call_registry_tool, call_stats,
};
use sovereign_contracts::ToolRegistry;
use sovereign_code::mcp_surface::{is_mcp_exposed, render_tools_list_gated, resolve_alias};

/// Code's registry over MCP. `tools/list` is spec-gated on `feature_root`:
/// `spec` and `drift` appear only once the project has a spec.
pub(crate) struct CodeTools {
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) session_id: String,
    pub(crate) feature_root: PathBuf,
}

impl McpToolHost for CodeTools {
    fn instructions(&self) -> Option<String> {
        None
    }

    fn list(&self) -> Value {
        Value::Array(render_tools_list_gated(
            &self.tools.descriptors(),
            Some(&self.feature_root),
        ))
    }

    async fn call(&self, name: &str, args: &Value, ctx: &McpRequestContext) -> Option<ToolOutcome> {
        let token = agent_session_token(ctx.agent_session.clone(), &self.session_id);
        call_registry_tool(
            &self.tools,
            resolve_alias(name),
            args,
            &self.session_id,
            token,
            is_mcp_exposed,
        )
        .await
    }
}

/// Every executed call goes to NoteStore's tool_call_log under this run's
/// session, then through the [`ToolPatternMatcher`], whose rules key on code
/// tool ids. Fire-and-forget: a log failure never touches the answer.
pub(crate) struct CodeCallLog {
    pub(crate) notes: Arc<NoteStore>,
    pub(crate) session_id: Arc<String>,
    pub(crate) matcher: Arc<ToolPatternMatcher>,
}

impl McpCallLog for CodeCallLog {
    fn record(&self, tool: &str, outcome: &ToolOutcome, _ctx: &McpRequestContext) {
        let Some(tag) = call_log_tag(outcome) else {
            tracing::debug!(tool, "mcp: no tool executed, nothing logged");
            return;
        };
        let tool = resolve_alias(tool).to_string();
        let notes = Arc::clone(&self.notes);
        let session = Arc::clone(&self.session_id);
        let matcher = Arc::clone(&self.matcher);
        tokio::spawn(async move {
            if let Err(e) = notes.log_tool_call(&session, &tool, tag).await {
                tracing::debug!(tool = %tool, error = %e, "mcp: tool_call_log write failed");
            }
            matcher.observe_and_record(session.as_str(), None).await;
        });
    }
}

/// GET /mcp/stats — tool call counts since server start; `svrn serve
/// --background` probes it for readiness.
pub(crate) async fn mcp_stats(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tools): Extension<Arc<ToolRegistry>>,
) -> axum::response::Response {
    if !peer.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "local-only"})),
        )
            .into_response();
    }
    (StatusCode::OK, Json(call_stats(&tools))).into_response()
}
