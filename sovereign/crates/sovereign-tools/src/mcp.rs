use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

/// MCP client that communicates with an MCP server via stdio (JSON-RPC).
pub struct McpClient {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
    #[allow(dead_code)]
    child: Arc<Mutex<Child>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl McpClient {
    /// Spawn an MCP server and initialize the connection.
    pub async fn connect(command: &str, args: &[&str]) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Error::Execution(format!("Failed to spawn MCP server: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Execution("No stdin on MCP process".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Execution("No stdout on MCP process".to_string()))?;

        let client = Self {
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            child: Arc::new(Mutex::new(child)),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        };

        // Send initialize request.
        let init_result = client
            .call(
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "sovereign",
                        "version": "0.1.0"
                    }
                }),
            )
            .await?;

        eprintln!("[mcp] Initialized: {}", init_result);

        // Send initialized notification.
        client.notify("notifications/initialized", serde_json::json!({})).await?;

        Ok(client)
    }

    /// Send a JSON-RPC request and wait for the response.
    async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&request)
            .map_err(|e| Error::Execution(format!("JSON serialization failed: {e}")))?;
        line.push('\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| Error::Execution(format!("Failed to write to MCP server: {e}")))?;
            stdin
                .flush()
                .await
                .map_err(|e| Error::Execution(format!("Failed to flush MCP stdin: {e}")))?;
        }

        // Read response line.
        let mut response_line = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            stdout
                .read_line(&mut response_line)
                .await
                .map_err(|e| Error::Execution(format!("Failed to read from MCP server: {e}")))?;
        }

        let response: serde_json::Value = serde_json::from_str(&response_line)
            .map_err(|e| Error::Execution(format!("Invalid JSON from MCP server: {e}")))?;

        if let Some(error) = response.get("error") {
            return Err(Error::Execution(format!("MCP error: {error}")));
        }

        Ok(response.get("result").cloned().unwrap_or(serde_json::json!(null)))
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&notification)
            .map_err(|e| Error::Execution(format!("JSON serialization failed: {e}")))?;
        line.push('\n');

        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| Error::Execution(format!("Failed to write notification: {e}")))?;
        stdin.flush().await.map_err(|e| Error::Execution(format!("Flush failed: {e}")))?;

        Ok(())
    }

    /// List available tools from the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        let result = self.call("tools/list", serde_json::json!({})).await?;

        let tools = result
            .get("tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::Execution("Invalid tools/list response".to_string()))?;

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

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<String> {
        let result = self
            .call(
                "tools/call",
                serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await?;

        // Extract text content from the response.
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

        Ok(content)
    }
}

/// Information about a tool from an MCP server.
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Wraps a single MCP tool as a Sovereign Tool.
pub struct McpToolAdapter {
    tool_name: String,
    description: String,
    tool_id: String,
    input_schema: serde_json::Value,
    client: Arc<McpClient>,
}

impl McpToolAdapter {
    pub fn new(info: &McpToolInfo, client: Arc<McpClient>, prefix: &str) -> Self {
        let tool_id = format!("mcp_{prefix}_{}", info.name);
        Self {
            tool_name: info.name.clone(),
            description: info.description.clone(),
            tool_id,
            input_schema: info.input_schema.clone(),
            client,
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
        let result = self.client.call_tool(&self.tool_name, params.clone()).await?;
        Ok(StepOutput::Text(result))
    }
}

/// Connect to an MCP server and return Tool implementations for each of its tools.
pub async fn connect_mcp_server(
    command: &str,
    args: &[&str],
    prefix: &str,
) -> Result<Vec<Box<dyn Tool>>> {
    let client = Arc::new(McpClient::connect(command, args).await?);
    let tools = client.list_tools().await?;

    eprintln!("[mcp] {} tools from {command}", tools.len());

    let adapters: Vec<Box<dyn Tool>> = tools
        .iter()
        .map(|info| {
            Box::new(McpToolAdapter::new(info, Arc::clone(&client), prefix)) as Box<dyn Tool>
        })
        .collect();

    Ok(adapters)
}
