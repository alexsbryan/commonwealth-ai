// SPDX-License-Identifier: AGPL-3.0-or-later
//! Transport abstraction for MCP communication.

use serde_json::Value;

/// Transport-level error type for MCP operations.
/// Separate from `sovereign_core::Error` — mapped at the adapter boundary.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Protocol error {code}: {message}")]
    Protocol { code: i32, message: String },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Server disconnected")]
    Disconnected,

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Tool execution failed: {0}")]
    ToolFailed(String),
}

/// The single extension point for MCP transports.
/// Implementations: `StdioTransport`, `HttpSseTransport`.
#[async_trait::async_trait]
pub trait McpTransport: Send + Sync + 'static {
    /// Send a JSON-RPC request and await the response.
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError>;

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError>;

    /// Clean disconnect.
    async fn close(&self) -> Result<(), McpError>;
}
