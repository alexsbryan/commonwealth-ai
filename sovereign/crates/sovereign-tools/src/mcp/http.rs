// SPDX-License-Identifier: AGPL-3.0-or-later
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

        let url =
            Url::parse(server_url).map_err(|e| McpError::Transport(format!("Invalid URL: {e}")))?;

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
        if let Some(session_endpoint) = result.get("sessionEndpoint").and_then(|v| v.as_str()) {
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
        let mut req = self
            .client
            .post(endpoint)
            // Streamable-HTTP servers (MCP spec) require the client to accept BOTH
            // application/json AND text/event-stream, or they reject with 406. Our
            // own reference server is lenient, but spec-compliant off-the-shelf
            // servers enforce it — so always send it.
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .json(&body);
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
            return Err(McpError::Transport(format!("HTTP {status}: {body}")));
        }

        // A streamable-HTTP POST is answered either with a single JSON object
        // (application/json) or an SSE stream (text/event-stream) whose `data:`
        // line carries the JSON-RPC message. Handle both, so off-the-shelf servers
        // work — not just our JSON-only reference server.
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|t| t.contains("text/event-stream"))
            .unwrap_or(false);
        let rpc_response: Value = if is_sse {
            let text = response
                .text()
                .await
                .map_err(|e| McpError::Transport(format!("read SSE body: {e}")))?;
            parse_sse_data(&text).ok_or_else(|| {
                McpError::Transport(format!("no JSON-RPC data in SSE response: {text}"))
            })?
        } else {
            response
                .json()
                .await
                .map_err(|e| McpError::Transport(format!("Invalid JSON response: {e}")))?
        };

        // Extract result or error from JSON-RPC envelope.
        if let Some(error) = rpc_response.get("error") {
            return Err(McpError::Protocol {
                code: error["code"].as_i64().unwrap_or(-1) as i32,
                message: error["message"].as_str().unwrap_or("unknown").to_string(),
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
        let mut req = self
            .client
            .post(endpoint)
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .json(&body);
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

/// Extract the JSON-RPC payload from an SSE response body: the last `data:` line
/// that parses as JSON. Streamable-HTTP MCP servers may answer a POST with an
/// event stream (`event: message\ndata: {…}`) instead of a plain JSON body — the
/// shape spec-compliant off-the-shelf servers (e.g. supergateway) actually send.
fn parse_sse_data(text: &str) -> Option<Value> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SSE framing a streamable-HTTP server replies with — `event:` +
    /// `data: {json}` — yields the JSON-RPC envelope. Pins the off-the-shelf
    /// interop fix (the filesystem server returns exactly this).
    #[test]
    fn parses_jsonrpc_from_sse_data_line() {
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let v = parse_sse_data(sse).expect("data line parses");
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 0);
        // A body with no JSON `data:` line is None (caller errors loudly).
        assert!(parse_sse_data("event: ping\n\n").is_none());
    }

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
