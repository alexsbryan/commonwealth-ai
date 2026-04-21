//! MCP (Model Context Protocol) client integration.
//!
//! Connects to MCP servers over stdio or HTTP+SSE transport,
//! discovers their tools, and exposes each tool as a native
//! Sovereign `Tool` implementation.

pub mod auth;
pub mod client;
pub mod config;
pub mod discovery;
pub mod http;
pub mod reconnect;
pub mod stdio;
pub mod transport;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub use config::McpServerConfig;
pub use discovery::McpServerManager;
pub use types::McpToolInfo;

// ─── McpToolCaller trait ──────────────────────────────────────

/// Object-safe interface for calling MCP tools, regardless of transport.
/// Implemented by `McpClient<T>` for any `T: McpTransport`.
#[async_trait]
pub trait McpToolCaller: Send + Sync {
    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> std::result::Result<String, transport::McpError>;
}

#[async_trait]
impl<T: transport::McpTransport> McpToolCaller for client::McpClient<T> {
    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> std::result::Result<String, transport::McpError> {
        self.call_tool(tool_name, arguments).await
    }
}

// ─── McpToolAdapter ───────────────────────────────────────────

/// Wraps a single MCP tool as a Sovereign Tool.
/// Tool name format: `mcp_{prefix}_{tool_name}`
/// Works with any transport via the `McpToolCaller` trait object.
pub struct McpToolAdapter {
    tool_name: String,
    description: String,
    tool_id: String,
    input_schema: serde_json::Value,
    caller: Arc<dyn McpToolCaller>,
}

impl McpToolAdapter {
    pub fn new(info: &McpToolInfo, caller: Arc<dyn McpToolCaller>, prefix: &str) -> Self {
        let tool_id = format!("mcp_{prefix}_{}", info.name);
        Self {
            tool_name: info.name.clone(),
            description: info.description.clone(),
            tool_id,
            input_schema: info.input_schema.clone(),
            caller,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn descriptor(&self) -> ToolDescriptor {
        let (effect, idempotency) = infer_behaviour(&self.tool_name, &self.description);
        ToolDescriptor {
            id: self.tool_id.clone(),
            name: self.tool_name.clone(),
            description: self.description.clone(),
            parameters: self.input_schema.clone(),
            examples: vec![],
            // External MCP servers don't declare behavioural properties
            // directly. We heuristically classify effect + idempotency
            // from the tool name + description (D4); unclassifiable
            // tools stay in the conservative quadrant so the executor
            // never auto-retries them and always audits writes.
            effect,
            idempotency,
            latency: Latency::Slow,
            scope: Scope::External,
            // External MCP servers don't declare output schemas; the
            // adapter reports None so downstream steps fall back to
            // `{N.output}` piping or reasoning. A future iteration
            // could inspect MCP `outputSchema` when the protocol
            // gains that field.
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Network]
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let result = self
            .caller
            .call_tool(&self.tool_name, params.clone())
            .await
            .map_err(|e| Error::Execution(format!("MCP tool call failed: {e}")))?;
        Ok(StepOutput::Text(result))
    }
}

/// Heuristic classification of an MCP tool's behavioural shape from
/// its name and description. External MCP servers don't declare these
/// properties; this is a best-guess that widens the safe retry set
/// beyond "everything is write-effectful" while keeping the
/// conservative default for anything ambiguous (D4).
///
/// Rules (checked in order):
///
/// 1. **Read-shaped name prefixes** — `get_`, `read_`, `list_`,
///    `find_`, `search_`, `show_`, `query_`, `fetch_`, `describe_`,
///    `lookup_`, `inspect_`, `count_`, `exists_` — classify as
///    `Effect::Read + Idempotency::Idempotent`. The vast majority of
///    MCP filesystem / git / documentation servers use these
///    prefixes for their read operations.
///
/// 2. **Destructive-name prefixes** — `delete_`, `remove_`, `drop_`,
///    `destroy_`, `purge_` — classify as `Effect::Write +
///    Idempotency::Idempotent` (these are by convention idempotent —
///    second delete is a no-op).
///
/// 3. **Mutating-name prefixes** — `create_`, `add_`, `insert_`,
///    `write_`, `post_`, `send_`, `publish_`, `push_`, `commit_`,
///    `apply_`, `update_`, `set_` — classify as `Effect::Write +
///    Idempotency::NonIdempotent` (second call creates a second
///    side-effect).
///
/// 4. **Fallback** — unclassified tools stay at the conservative
///    default `Effect::Write + Idempotency::NonIdempotent`.
///
/// No parameter-schema inspection yet — the name signal is high-
/// precision on its own and schema inference would need MCP-server
/// conventions we don't have a corpus for.
fn infer_behaviour(name: &str, _description: &str) -> (Effect, Idempotency) {
    const READ_PREFIXES: &[&str] = &[
        "get_", "read_", "list_", "find_", "search_", "show_", "query_", "fetch_",
        "describe_", "lookup_", "inspect_", "count_", "exists_",
    ];
    const DESTRUCTIVE_PREFIXES: &[&str] = &["delete_", "remove_", "drop_", "destroy_", "purge_"];
    const MUTATING_PREFIXES: &[&str] = &[
        "create_", "add_", "insert_", "write_", "post_", "send_", "publish_", "push_",
        "commit_", "apply_", "update_", "set_",
    ];

    let lower = name.to_lowercase();

    if READ_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return (Effect::Read, Idempotency::Idempotent);
    }
    if DESTRUCTIVE_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return (Effect::Write, Idempotency::Idempotent);
    }
    if MUTATING_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return (Effect::Write, Idempotency::NonIdempotent);
    }

    // Unknown shape — conservative default.
    (Effect::Write, Idempotency::NonIdempotent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_prefixes_infer_read_idempotent() {
        for name in ["read_file", "get_status", "list_branches", "find_symbol", "search_docs"] {
            let (e, i) = infer_behaviour(name, "");
            assert_eq!(e, Effect::Read, "name={name}");
            assert_eq!(i, Idempotency::Idempotent, "name={name}");
        }
    }

    #[test]
    fn destructive_prefixes_are_write_but_idempotent() {
        for name in ["delete_file", "remove_tag", "drop_table", "purge_cache"] {
            let (e, i) = infer_behaviour(name, "");
            assert_eq!(e, Effect::Write, "name={name}");
            assert_eq!(
                i,
                Idempotency::Idempotent,
                "delete-family is idempotent by convention (name={name})"
            );
        }
    }

    #[test]
    fn mutating_prefixes_are_write_nonidempotent() {
        for name in ["create_issue", "add_user", "send_email", "post_comment", "commit_tx"] {
            let (e, i) = infer_behaviour(name, "");
            assert_eq!(e, Effect::Write, "name={name}");
            assert_eq!(i, Idempotency::NonIdempotent, "name={name}");
        }
    }

    #[test]
    fn unclassified_names_stay_conservative() {
        // Deliberately obscure so no prefix matches.
        for name in ["execute", "run", "process", "something_weird"] {
            let (e, i) = infer_behaviour(name, "");
            assert_eq!(
                e,
                Effect::Write,
                "fallback must be conservative (name={name})"
            );
            assert_eq!(
                i,
                Idempotency::NonIdempotent,
                "fallback must be conservative (name={name})"
            );
        }
    }

    #[test]
    fn prefix_match_is_case_insensitive() {
        let (e, _) = infer_behaviour("READ_FILE", "");
        assert_eq!(e, Effect::Read);
    }
}

// ─── Public helpers ───────────────────────────────────────────

/// Connect to a stdio MCP server and return Tool implementations.
/// Preserved API for backward compatibility.
pub async fn connect_mcp_server(
    command: &str,
    args: &[&str],
    prefix: &str,
) -> Result<Vec<Box<dyn Tool>>> {
    let transport = stdio::StdioTransport::spawn(command, args)
        .await
        .map_err(|e| Error::Execution(format!("MCP spawn failed: {e}")))?;
    connect_and_wrap(transport, prefix).await
}

/// Connect to an HTTP MCP server and return Tool implementations.
pub async fn connect_http_mcp_server(
    url: &str,
    auth: auth::McpAuth,
    prefix: &str,
) -> Result<Vec<Box<dyn Tool>>> {
    let transport = http::HttpSseTransport::connect(url, auth)
        .await
        .map_err(|e| Error::Execution(format!("MCP HTTP connect failed: {e}")))?;
    connect_and_wrap(transport, prefix).await
}

/// Generic: connect via any transport, discover tools, wrap as Tool objects.
async fn connect_and_wrap<T: transport::McpTransport>(
    transport: T,
    prefix: &str,
) -> Result<Vec<Box<dyn Tool>>> {
    let mcp_client = client::McpClient::connect(transport, prefix)
        .await
        .map_err(|e| Error::Execution(format!("MCP connect failed: {e}")))?;

    let tools = mcp_client
        .list_tools()
        .await
        .map_err(|e| Error::Execution(format!("MCP list_tools failed: {e}")))?;

    eprintln!("[mcp] {} tools from {prefix}", tools.len());

    let caller: Arc<dyn McpToolCaller> = Arc::new(mcp_client);
    let adapters: Vec<Box<dyn Tool>> = tools
        .iter()
        .map(|info| {
            Box::new(McpToolAdapter::new(info, Arc::clone(&caller), prefix)) as Box<dyn Tool>
        })
        .collect();

    Ok(adapters)
}
