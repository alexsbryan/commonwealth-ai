// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for the Settings → MCP Servers pane.
//!
//! MCP servers live in the canonical `~/.sovereign/config.toml` (`SetupConfig`)
//! — the exact same `[[mcp_servers]]` list `sovereign chat` and `sovereign
//! serve` read, so a server added here is available on every surface. These
//! commands mutate that one file; the new server is connected on the next
//! backend start by the bootstrap loader (`load_from_setup_config`). HTTP-only
//! by design — Sovereign does not spawn/supervise stdio subprocesses.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sovereign_core::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
use sovereign_core::setup_config::SetupConfig;
use sovereign_tools::mcp::auth::{secret_env_var, McpAuth};
use tauri::State;

use crate::state::AppState;

/// One MCP server as shown in the settings pane: its config plus the live
/// connection status captured at the last bootstrap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub url: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub bearer: bool,
    /// Env var the bearer token is read from, so the UI can tell the user
    /// exactly what to set. `None` for no-auth servers.
    pub token_env: Option<String>,
    /// Live status from bootstrap: `Some(true)` connected, `Some(false)`
    /// failed, `None` if the backend hasn't connected this server yet (e.g.
    /// added since the last start — restart to load it).
    pub connected: Option<bool>,
    pub tool_count: Option<usize>,
    pub error: Option<String>,
}

fn http_url(t: &McpTransportConfig) -> String {
    match t {
        McpTransportConfig::Http { url, .. } => url.clone(),
        McpTransportConfig::Stdio { command, .. } => format!("stdio:{command}"),
    }
}

fn is_bearer(t: &McpTransportConfig) -> bool {
    matches!(
        t,
        McpTransportConfig::Http {
            auth: McpAuthConfig::Bearer,
            ..
        }
    )
}

/// List configured MCP servers, annotated with live bootstrap status.
#[tauri::command]
pub async fn mcp_list_servers(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<McpServerView>, String> {
    let servers = SetupConfig::load().map(|c| c.mcp_servers).unwrap_or_default();
    let statuses = match state.mcp_servers.read().await.as_ref() {
        Some(mgr) => mgr.server_statuses().await,
        None => Vec::new(),
    };
    let view = servers
        .into_iter()
        .map(|s| {
            let st = statuses.iter().find(|x| x.name == s.name);
            let bearer = is_bearer(&s.transport);
            let token_env = bearer.then(|| secret_env_var(&s.name));
            let url = http_url(&s.transport);
            let connected = st.map(|x| x.connected);
            let tool_count = st.map(|x| x.tool_count);
            let error = st.and_then(|x| x.error.clone());
            McpServerView {
                name: s.name,
                url,
                description: s.description,
                enabled: s.enabled,
                bearer,
                token_env,
                connected,
                tool_count,
                error,
            }
        })
        .collect();
    Ok(view)
}

/// Add (or replace, by name) an HTTP MCP server in the canonical config.
#[tauri::command]
pub async fn mcp_add_server(
    name: String,
    url: String,
    description: Option<String>,
    bearer: bool,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Server name is required.".into());
    }
    if url.trim().is_empty() {
        return Err("Server URL is required.".into());
    }
    let mut cfg = SetupConfig::load()
        .map_err(|e| format!("Could not load config ({e}). Finish setup first."))?;
    let auth = if bearer {
        McpAuthConfig::Bearer
    } else {
        McpAuthConfig::None
    };
    let entry = McpServerConfig {
        name: name.clone(),
        description: description.filter(|d| !d.trim().is_empty()),
        enabled: true,
        transport: McpTransportConfig::Http { url, auth },
        global: true,
    };
    cfg.mcp_servers.retain(|s| s.name != name);
    cfg.mcp_servers.push(entry);
    cfg.save().map_err(|e| format!("save config: {e}"))?;
    Ok(())
}

/// Remove a server from the canonical config.
#[tauri::command]
pub async fn mcp_remove_server(name: String) -> Result<(), String> {
    let mut cfg = SetupConfig::load().map_err(|e| format!("load config: {e}"))?;
    let before = cfg.mcp_servers.len();
    cfg.mcp_servers.retain(|s| s.name != name);
    if cfg.mcp_servers.len() == before {
        return Err(format!("No MCP server named '{name}'."));
    }
    cfg.save().map_err(|e| format!("save config: {e}"))?;
    Ok(())
}

/// Probe an HTTP MCP server without persisting it — returns the tool count so
/// the user gets immediate "is this reachable?" feedback in the add dialog.
#[tauri::command]
pub async fn mcp_test_connection(name: String, url: String, bearer: bool) -> Result<usize, String> {
    let auth = if bearer {
        McpAuth::resolve(&name, &McpAuthConfig::Bearer)
    } else {
        McpAuth::None
    };
    let tools = sovereign_tools::mcp::connect_http_mcp_server(&url, auth, &name)
        .await
        .map_err(|e| e.to_string())?;
    Ok(tools.len())
}
