// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reconnecting transport wrapper.
//!
//! Wraps an `HttpSseTransport` with automatic reconnection on disconnect.
//! Transparent to callers — they never see a disconnected state.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;

use super::auth::McpAuth;
use super::http::HttpSseTransport;
use super::transport::{McpError, McpTransport};

/// Wraps an HTTP transport with automatic reconnection on disconnect.
pub struct ReconnectingTransport {
    url: String,
    auth: McpAuth,
    inner: Arc<RwLock<Option<HttpSseTransport>>>,
    max_retries: usize,
}

impl ReconnectingTransport {
    /// Create a reconnecting transport. Performs the initial connection.
    pub async fn connect(url: &str, auth: McpAuth, max_retries: usize) -> Result<Self, McpError> {
        let transport = HttpSseTransport::connect(url, auth.clone()).await?;
        Ok(Self {
            url: url.to_string(),
            auth,
            inner: Arc::new(RwLock::new(Some(transport))),
            max_retries,
        })
    }

    /// Ensure the inner transport is connected. Reconnects if needed.
    async fn ensure_connected(&self) -> Result<(), McpError> {
        {
            let inner = self.inner.read().await;
            if inner.is_some() {
                return Ok(());
            }
        }

        // Need to reconnect.
        let mut inner = self.inner.write().await;
        // Double-check after acquiring write lock.
        if inner.is_some() {
            return Ok(());
        }

        tracing::info!(url = &self.url, "MCP reconnecting...");
        let transport = HttpSseTransport::connect(&self.url, self.auth.clone()).await?;
        *inner = Some(transport);
        tracing::info!(url = &self.url, "MCP reconnected");
        Ok(())
    }
}

#[async_trait::async_trait]
impl McpTransport for ReconnectingTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        self.ensure_connected().await?;

        let result = {
            let inner = self.inner.read().await;
            inner
                .as_ref()
                .unwrap()
                .request(method, params.clone())
                .await
        };

        match result {
            Err(McpError::Disconnected) | Err(McpError::Transport(_)) => {
                // Clear the transport and retry once.
                {
                    let mut inner = self.inner.write().await;
                    *inner = None;
                }

                for attempt in 0..self.max_retries {
                    let delay = std::time::Duration::from_millis(500 * (1 << attempt));
                    tokio::time::sleep(delay).await;

                    match self.ensure_connected().await {
                        Ok(()) => {
                            let inner = self.inner.read().await;
                            return inner
                                .as_ref()
                                .unwrap()
                                .request(method, params.clone())
                                .await;
                        }
                        Err(e) => {
                            tracing::warn!(
                                attempt = attempt + 1,
                                max = self.max_retries,
                                error = %e,
                                "MCP reconnection attempt failed"
                            );
                        }
                    }
                }

                Err(McpError::Disconnected)
            }
            other => other,
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.ensure_connected().await?;
        let inner = self.inner.read().await;
        inner.as_ref().unwrap().notify(method, params).await
    }

    async fn close(&self) -> Result<(), McpError> {
        let mut inner = self.inner.write().await;
        if let Some(transport) = inner.take() {
            transport.close().await
        } else {
            Ok(())
        }
    }
}
