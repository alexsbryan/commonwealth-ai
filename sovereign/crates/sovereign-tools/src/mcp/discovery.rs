//! MCP server discovery and lifecycle management.
//!
//! `McpServerManager` owns all active MCP connections, connects to
//! configured servers at startup, and registers discovered tools.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use sovereign_core::error::{Error, Result};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::Tool;

use super::auth::McpAuth;
use super::config::{McpAuthConfig, McpServerConfig, McpTransportConfig};

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
            let auth = resolve_auth(&config.name, auth_config);
            super::connect_http_mcp_server(url, auth, &config.name).await
        }
    }
}

/// Resolve auth configuration to an actual `McpAuth` value.
/// For keychain-backed auth, loads the credential from the keychain.
fn resolve_auth(server_name: &str, config: &McpAuthConfig) -> McpAuth {
    match config {
        McpAuthConfig::None => McpAuth::None,
        McpAuthConfig::Bearer => {
            // Try loading from keychain; fall back to None.
            #[cfg(feature = "keychain")]
            {
                McpAuth::from_keychain(server_name).unwrap_or(McpAuth::None)
            }
            #[cfg(not(feature = "keychain"))]
            {
                let _ = server_name;
                tracing::warn!("Keychain support not enabled — bearer auth unavailable");
                McpAuth::None
            }
        }
        McpAuthConfig::ApiKey { header } => {
            #[cfg(feature = "keychain")]
            {
                match McpAuth::from_keychain(server_name) {
                    Ok(McpAuth::ApiKey { value, .. }) => McpAuth::ApiKey {
                        header: header.clone(),
                        value,
                    },
                    _ => McpAuth::None,
                }
            }
            #[cfg(not(feature = "keychain"))]
            {
                let _ = (server_name, header);
                McpAuth::None
            }
        }
        McpAuthConfig::Basic => {
            #[cfg(feature = "keychain")]
            {
                McpAuth::from_keychain(server_name).unwrap_or(McpAuth::None)
            }
            #[cfg(not(feature = "keychain"))]
            {
                let _ = server_name;
                McpAuth::None
            }
        }
    }
}
