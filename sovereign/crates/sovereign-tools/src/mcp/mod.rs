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
        ToolDescriptor {
            id: self.tool_id.clone(),
            name: self.tool_name.clone(),
            description: self.description.clone(),
            parameters: self.input_schema.clone(),
            examples: vec![],
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
