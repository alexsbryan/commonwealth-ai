//! Generic MCP client over any transport.

use super::transport::{McpError, McpTransport};
use super::types::McpToolInfo;

/// MCP client generic over transport. Handles the MCP protocol
/// (initialize, tools/list, tools/call) on top of any transport.
pub struct McpClient<T: McpTransport> {
    transport: T,
    server_name: String,
}

impl<T: McpTransport> McpClient<T> {
    /// Create a client and run the MCP initialize handshake.
    pub async fn connect(transport: T, server_name: &str) -> Result<Self, McpError> {
        let client = Self {
            transport,
            server_name: server_name.to_string(),
        };

        // Initialize handshake.
        let _init_result = client
            .transport
            .request(
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "sovereign",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;

        // Send initialized notification.
        client
            .transport
            .notify("notifications/initialized", serde_json::json!({}))
            .await?;

        Ok(client)
    }

    /// Fetch the server's tool list.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let result = self
            .transport
            .request("tools/list", serde_json::json!({}))
            .await?;

        let tools = result
            .get("tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                McpError::Protocol {
                    code: -32600,
                    message: "Invalid tools/list response".into(),
                }
            })?;

        let mut infos = Vec::new();
        for tool in tools {
            let name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let description = tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            infos.push(McpToolInfo {
                name,
                description,
                input_schema,
            });
        }

        Ok(infos)
    }

    /// Execute a tool call.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, McpError> {
        let result = self
            .transport
            .request(
                "tools/call",
                serde_json::json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await?;

        // Extract text content from the MCP response.
        let content = result
            .get("content")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                            item.get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result.to_string());

        // Check if the tool reported an error.
        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(McpError::ToolFailed(content));
        }

        Ok(content)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Access the underlying transport (for close, etc.).
    pub fn transport(&self) -> &T {
        &self.transport
    }
}
