// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single entry point every chat surface calls to load configured MCP
//! servers into its tool registry.
//!
//! Reads the `[[mcp_servers]]` array from the canonical
//! `~/.sovereign/config.toml` ([`SetupConfig`]) and registers each enabled
//! server's tools. `sovereign chat`, the desktop bootstrap, and
//! `sovereign serve` all call this one function, so a server added in any
//! surface is available in all of them — parity by construction, the same
//! discipline as `sovereign_core::router_bootstrap::build_llm_router`.

use sovereign_core::registry::ToolRegistry;
use sovereign_core::setup_config::SetupConfig;

use super::McpServerManager;

/// Load all enabled MCP servers from the canonical `SetupConfig` into
/// `registry`, returning the manager (held for status display; the live
/// transports are owned by the registered tools, so a caller that only needs
/// the tools may drop it).
///
/// Never fails the caller: a missing/unreadable config yields an empty server
/// set, and `McpServerManager::from_config` connects each server independently
/// with a bounded timeout — a dead URL is logged and skipped, never aborting
/// startup.
pub async fn load_from_setup_config(registry: &mut ToolRegistry) -> McpServerManager {
    let servers = SetupConfig::load()
        .map(|c| c.mcp_servers)
        .unwrap_or_default();
    let enabled = servers.iter().filter(|s| s.enabled).count();
    if enabled > 0 {
        tracing::info!(
            configured = servers.len(),
            enabled,
            "mcp loader: connecting configured MCP servers"
        );
    }
    McpServerManager::from_config(&servers, registry).await
}
