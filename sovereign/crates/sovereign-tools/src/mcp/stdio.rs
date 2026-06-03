//! Stdio-based MCP transport.
//!
//! Spawns an MCP server as a child process and communicates via
//! stdin/stdout using line-delimited JSON-RPC.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::transport::{McpError, McpTransport};

/// Stdio transport: communicates with an MCP server via stdin/stdout.
pub struct StdioTransport {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
    #[allow(dead_code)]
    child: Arc<Mutex<Child>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl StdioTransport {
    /// Spawn an MCP server process.
    pub async fn spawn(command: &str, args: &[&str]) -> Result<Self, McpError> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| McpError::Transport(format!("Failed to spawn MCP server: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("No stdin on MCP process".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("No stdout on MCP process".into()))?;

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            child: Arc::new(Mutex::new(child)),
            next_id: std::sync::atomic::AtomicU64::new(1),
        })
    }
}

#[async_trait::async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&request)?;
        line.push('\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| McpError::Transport(format!("Write failed: {e}")))?;
            stdin
                .flush()
                .await
                .map_err(|e| McpError::Transport(format!("Flush failed: {e}")))?;
        }

        let mut response_line = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            stdout
                .read_line(&mut response_line)
                .await
                .map_err(|e| McpError::Transport(format!("Read failed: {e}")))?;
        }

        if response_line.is_empty() {
            return Err(McpError::Disconnected);
        }

        let response: Value = serde_json::from_str(&response_line)?;

        if let Some(error) = response.get("error") {
            return Err(McpError::Protocol {
                code: error["code"].as_i64().unwrap_or(-1) as i32,
                message: error["message"].as_str().unwrap_or("unknown").to_string(),
            });
        }

        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::json!(null)))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&notification)?;
        line.push('\n');

        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(format!("Write failed: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport(format!("Flush failed: {e}")))?;

        Ok(())
    }

    async fn close(&self) -> Result<(), McpError> {
        // Best-effort: the child process will be killed on drop.
        Ok(())
    }
}
