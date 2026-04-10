//! HTTP+SSE transport for MCP servers.
//!
//! Connects to remote MCP servers (SaaS tools like GitHub, Linear, Notion)
//! over HTTP, using JSON-RPC POST requests and optionally SSE for
//! server-initiated notifications.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use tokio::sync::Mutex;

use super::auth::McpAuth;
use super::transport::{McpError, McpTransport};

/// HTTP+SSE transport for remote MCP servers.
pub struct HttpSseTransport {
    /// The endpoint URL (may be updated by the initialize handshake).
    endpoint: Arc<Mutex<Url>>,
    auth: McpAuth,
    client: Client,
    next_id: AtomicU64,
}

impl HttpSseTransport {
    /// Connect to an MCP server over HTTP and run the initialize handshake.
    pub async fn connect(server_url: &str, auth: McpAuth) -> Result<Self, McpError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| McpError::Transport(format!("HTTP client error: {e}")))?;

        let url = Url::parse(server_url)
            .map_err(|e| McpError::Transport(format!("Invalid URL: {e}")))?;

        let transport = Self {
            endpoint: Arc::new(Mutex::new(url)),
            auth,
            client,
            next_id: AtomicU64::new(1),
        };

        // Run the MCP initialize handshake.
        let result = transport
            .request(
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "roots": { "listChanged": false },
                        "sampling": {}
                    },
                    "clientInfo": {
                        "name": "sovereign",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;

        // Some servers return a session endpoint in the response.
        if let Some(session_endpoint) = result
            .get("sessionEndpoint")
            .and_then(|v| v.as_str())
        {
            let mut ep = transport.endpoint.lock().await;
            *ep = Url::parse(session_endpoint)
                .map_err(|e| McpError::Transport(format!("Invalid session endpoint: {e}")))?;
        }

        // Send the initialized notification.
        transport
            .notify("notifications/initialized", serde_json::json!({}))
            .await?;

        Ok(transport)
    }
}

#[async_trait::async_trait]
impl McpTransport for HttpSseTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id":      id,
            "method":  method,
            "params":  params,
        });

        let endpoint = self.endpoint.lock().await.clone();
        let mut req = self.client.post(endpoint).json(&body);
        req = self.auth.inject(req);

        let response = req
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("HTTP request failed: {e}")))?;

        let status = response.status();

        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(McpError::AuthFailed(format!(
                "Server returned {}. Check your credentials.",
                status
            )));
        }

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!(
                "HTTP {status}: {body}"
            )));
        }

        let rpc_response: Value = response
            .json()
            .await
            .map_err(|e| McpError::Transport(format!("Invalid JSON response: {e}")))?;

        // Extract result or error from JSON-RPC envelope.
        if let Some(error) = rpc_response.get("error") {
            return Err(McpError::Protocol {
                code: error["code"].as_i64().unwrap_or(-1) as i32,
                message: error["message"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
            });
        }

        rpc_response
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::Protocol {
                code: -32600,
                message: "Response missing 'result' field".into(),
            })
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method":  method,
            "params":  params,
        });

        let endpoint = self.endpoint.lock().await.clone();
        let mut req = self.client.post(endpoint).json(&body);
        req = self.auth.inject(req);

        let response = req
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("Notification failed: {e}")))?;

        // Notifications may return empty body or 202 Accepted.
        // Don't error on non-200 as long as it's not a clear failure.
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(McpError::AuthFailed("Notification auth failed".into()));
        }

        Ok(())
    }

    async fn close(&self) -> Result<(), McpError> {
        // Best-effort shutdown notification.
        let _ = self
            .notify(
                "notifications/cancelled",
                serde_json::json!({
                    "requestId": null,
                    "reason": "client disconnect"
                }),
            )
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_transport_url_parsing() {
        let url = Url::parse("https://api.example.com/mcp/");
        assert!(url.is_ok());
    }

    #[test]
    fn http_transport_invalid_url() {
        let url = Url::parse("not a url");
        assert!(url.is_err());
    }
}
