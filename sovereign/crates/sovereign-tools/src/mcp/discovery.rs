//! MCP server discovery and lifecycle management.
//!
//! `McpServerManager` owns all active MCP connections, connects to
//! configured servers at startup, and registers discovered tools.

use tokio::sync::RwLock;

use sovereign_core::error::Result;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::Tool;

use super::auth::McpAuth;
use super::config::{McpServerConfig, McpTransportConfig};

/// Connection status for a server.
#[derive(Debug, Clone)]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub tool_count: usize,
    pub error: Option<String>,
}

/// Manages all active MCP server connections.
pub struct McpServerManager {
    statuses: RwLock<Vec<McpServerStatus>>,
}

impl McpServerManager {
    /// Connect to all enabled servers and register their tools.
    pub async fn from_config(
        configs: &[McpServerConfig],
        registry: &mut ToolRegistry,
    ) -> Self {
        let mut statuses = Vec::new();

        for config in configs {
            if !config.enabled {
                continue;
            }

            match connect_and_discover(config).await {
                Ok(tools) => {
                    let count = tools.len();
                    tracing::info!(
                        server = &config.name,
                        tool_count = count,
                        "MCP server connected"
                    );
                    for tool in tools {
                        registry.register(tool);
                    }
                    statuses.push(McpServerStatus {
                        name: config.name.clone(),
                        connected: true,
                        tool_count: count,
                        error: None,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        server = &config.name,
                        error = %e,
                        "MCP server connection failed"
                    );
                    statuses.push(McpServerStatus {
                        name: config.name.clone(),
                        connected: false,
                        tool_count: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Self {
            statuses: RwLock::new(statuses),
        }
    }

    /// Get the status of all configured servers.
    pub async fn server_statuses(&self) -> Vec<McpServerStatus> {
        self.statuses.read().await.clone()
    }
}

/// Connect to a single server and discover its tools.
async fn connect_and_discover(
    config: &McpServerConfig,
) -> Result<Vec<Box<dyn Tool>>> {
    match &config.transport {
        McpTransportConfig::Stdio { command, args, .. } => {
            let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            super::connect_mcp_server(command, &args_refs, &config.name).await
        }
        McpTransportConfig::Http { url, auth: auth_config } => {
            let auth = McpAuth::resolve(&config.name, auth_config);
            super::connect_http_mcp_server(url, auth, &config.name).await
        }
    }
}
