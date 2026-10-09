// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code's two MCP ports (phase-b pb-code-server): [`CodeTools`], the host
//! kit's tool host over code's registry, and [`CodeCallLog`], its call log
//! over the NoteStore that `svrn reflect` and the pattern matcher read. The
//! ToolRegistry half of a call is sovereign-contracts' `mcp_host`, the one
//! svrn's daemon runs too; what code exposes is code's own list,
//! [`crate::mcp_surface`]. Moved with code's face from sovereign-cli-dev's
//! `project_cmd::mcp_host` (pb-code-daemon-exit), which re-exports both.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine_notes::mining::patterns::ToolPatternMatcher;
use corpus_engine_notes::NoteStore;
use host_kit::mcp::{McpCallLog, McpRequestContext, McpToolHost, ToolOutcome};
use serde_json::Value;
use sovereign_contracts::mcp_host::{call_log_tag, call_registry_tool};
use sovereign_contracts::ToolRegistry;

use crate::mcp_surface::{is_mcp_exposed, render_tools_list_gated_by, resolve_alias};

/// Code's registry over MCP. `tools/list` is spec-gated on `feature_root`:
/// `spec` and `drift` appear only once the project has a spec. A connection
/// that names a corpus (`x-svrn-corpus`) is admitted only when `indexes_dir`
/// holds that code corpus.
pub struct CodeTools {
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) session_id: String,
    pub(crate) feature_root: Option<PathBuf>,
    pub(crate) indexes_dir: PathBuf,
}

impl McpToolHost for CodeTools {
    fn instructions(&self) -> Option<String> {
        None
    }

    fn list(&self, ctx: &McpRequestContext) -> Value {
        Value::Array(render_tools_list_gated_by(
            &self.tools.descriptors(),
            self.feature_root.as_deref(),
            ctx.read_only,
            is_mcp_exposed,
        ))
    }

    fn admit(&self, ctx: &McpRequestContext) -> Result<(), String> {
        crate::admit_corpus(&self.indexes_dir, ctx.corpus.as_deref()).map_err(|why| {
            format!(
                "{}: {why}",
                sovereign_contracts::oicp::mcp::MCP_CORPUS_HEADER
            )
        })
    }

    async fn call(&self, name: &str, args: &Value, ctx: &McpRequestContext) -> Option<ToolOutcome> {
        call_registry_tool(
            &self.tools,
            resolve_alias(name),
            args,
            &self.session_id,
            ctx,
            is_mcp_exposed,
        )
        .await
    }
}

/// Every executed call goes to NoteStore's tool_call_log under this run's
/// session, then through the [`ToolPatternMatcher`], whose rules key on code
/// tool ids. Fire-and-forget: a log failure never touches the answer.
pub struct CodeCallLog {
    pub(crate) notes: Arc<NoteStore>,
    pub(crate) session_id: Arc<String>,
    pub(crate) matcher: Arc<ToolPatternMatcher>,
}

impl McpCallLog for CodeCallLog {
    fn record(&self, tool: &str, outcome: &ToolOutcome, ctx: &McpRequestContext) {
        let Some(tag) = call_log_tag(outcome) else {
            tracing::debug!(tool, "mcp: no tool executed, nothing logged");
            return;
        };
        let tool = resolve_alias(tool).to_string();
        let notes = Arc::clone(&self.notes);
        let session = Arc::clone(&self.session_id);
        let matcher = Arc::clone(&self.matcher);
        let caller = ctx.caller.clone();
        tokio::spawn(async move {
            if let Err(e) = notes
                .log_tool_call_by(&session, &tool, tag, caller.as_deref())
                .await
            {
                tracing::debug!(tool = %tool, error = %e, "mcp: tool_call_log write failed");
            }
            matcher.observe_and_record(session.as_str(), None).await;
        });
    }
}
