// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for the Settings → MCP Servers pane.
//!
//! MCP servers live in the canonical `~/.svrnmesh/config.toml` (`SetupConfig`)
//! — the exact same `[[mcp_servers]]` list `sovereign chat` and `sovereign
//! serve` read, so a server added here is available on every surface.
//! HTTP-only by design — svrnmesh does not spawn/supervise stdio subprocesses.
//!
//! # Where the server list is decided (sv-surface D8)
//!
//! `mcp_list_servers`, `mcp_test_connection`, `mcp_set_token` and
//! `mcp_clear_token` hold no `SetupConfig` and no secret store. Each is one
//! call onto `sovereign_mesh::mcp_config_http`'s `/v1/mcp/servers` routes
//! over [`mcp_client`]. On an attached boot the pane used to render THIS
//! process's config file and THIS process's secret dir while the daemon
//! connected servers out of its own — two answers to one question.
//!
//! # What the host cannot say, and what this does with that (ARCH §18.3)
//!
//! The route serves no `connected` and no `error`: the runtime recipe drops
//! the `McpServerManager` after boot, so the daemon holds no connection
//! manager to ask, and it says so in `mount.reason` on every response. What
//! it CAN observe is `live_tool_count` — tools in its own registry carrying
//! this server's `mcp_<name>_` prefix — plus whether there was a registry to
//! count against at all (`mount.mounted`).
//!
//! So the DTO below keeps its field names and fills them only where the host
//! knows:
//!
//!   * `tool_count` = `live_tool_count`, and only when the mount exists.
//!     `None` means "not counted", never "zero tools".
//!   * `connected` = `None`, always. `connected = tool_count > 0` is exactly
//!     the fact the host declined to invent, and inventing it here would put
//!     a green dot on a server nobody has dialled.
//!   * `error` = `None`, always — there is no probe to have failed.
//!
//! USER-VISIBLE CONSEQUENCE, named rather than absorbed: a configured server
//! now shows the pane's `connected === null` line ("not loaded — restart to
//! connect") instead of "connected · N tools", because nothing in this
//! process has dialled it. The counts the host does hold are on the wire and
//! in the log below; rendering them under a third status state is a frontend
//! change (`McpServersSection.svelte::statusLabel`), owed and out of this
//! file's scope.
//!
//! `mcp_add_server` / `mcp_remove_server` STAY app-local, measured: they call
//! `SetupConfig::save()`, which has zero call sites in `sovereign-mesh` —
//! every config writer in this workspace is CLI-side. A `POST
//! /v1/admin/config` comes before they can cross, and until then a write sent
//! to a daemon that cannot persist it would be a silent no-op.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sovereign_core::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
use sovereign_core::setup_config::SetupConfig;
use sovereign_tools::mcp::secret_store;
use tauri::State;

use crate::state::AppState;

/// The client for the daemon's MCP-config surface — the same
/// `config.toml` and the same secret store, reached over loopback
/// instead (sv-surface D8).
fn mcp_client(state: &AppState) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
}

/// One MCP server as shown in the settings pane. The first seven fields are
/// the host's `McpServerView` verbatim; the last three are this DTO's own
/// and are filled only as far as the host can honestly speak — see the
/// module header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub url: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub bearer: bool,
    /// Env var the bearer token is read from — shown as the headless / CI
    /// override. `None` for no-auth servers.
    pub token_env: Option<String>,
    /// Whether a token is currently stored in the secret file for this
    /// server (the primary path). Drives the "token set" affordance.
    pub has_token: bool,
    /// ALWAYS `None`. The host keeps no connection manager, and a connect
    /// flag folded out of a tool count is a fact nobody measured.
    pub connected: Option<bool>,
    /// Tools in the host's live registry carrying this server's prefix, when
    /// there was a registry to count against. `None` is "not counted".
    pub tool_count: Option<usize>,
    /// ALWAYS `None` — no probe ran, so none can have failed.
    pub error: Option<String>,
}

/// List configured MCP servers, annotated with what the daemon's own tool
/// registry holds for each.
#[tauri::command]
pub async fn mcp_list_servers(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<McpServerView>, String> {
    let resp = mcp_client(&state)
        .mcp_servers::<sovereign_mesh::mcp_config_http::McpServersResponse>()
        .await
        .map_err(|e| format!("mcp_list_servers: {e}"))?;
    // Glassbox: the host states the absence on every response, and the pane
    // cannot render it yet. Log it so a "why is nothing connected?" question
    // has an answer at `tracing=debug` (ARCH §9.1).
    tracing::debug!(
        mounted = resp.mount.mounted,
        total_tools = resp.mount.total_tools,
        servers = resp.servers.len(),
        reason = %resp.mount.reason,
        "mcp_list_servers: host served counts, not connect status"
    );
    let mounted = resp.mount.mounted;
    Ok(resp
        .servers
        .into_iter()
        .map(|s| McpServerView {
            name: s.name,
            url: s.url,
            description: s.description,
            enabled: s.enabled,
            bearer: s.bearer,
            token_env: s.token_env,
            has_token: s.has_token,
            connected: None,
            tool_count: mounted.then_some(s.live_tool_count),
            error: None,
        })
        .collect())
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
    // Don't leave an orphaned secret behind.
    let _ = secret_store::delete_token(&name);
    Ok(())
}

/// Probe an HTTP MCP server without persisting it — returns the tool count so
/// the user gets immediate "is this reachable?" feedback in the add dialog.
///
/// The HOST dials it, which is what makes the answer mean anything: on an
/// attached boot this process's network position is not the daemon's, and a
/// server reachable from here that the daemon cannot see would have tested
/// green and then served nothing.
#[tauri::command]
pub async fn mcp_test_connection(
    state: State<'_, Arc<AppState>>,
    name: String,
    url: String,
    bearer: bool,
    token: Option<String>,
) -> Result<usize, String> {
    // The token the user just typed (not yet saved) so "Test" reflects the
    // form; absent, the host falls back to its stored / env secret. The
    // blank-vs-absent distinction is the host's now, one decider.
    let typed = token.as_deref().map(str::trim).filter(|t| !t.is_empty());
    mcp_client(&state)
        .mcp_test_connection(&name, &url, bearer, typed)
        .await
        .map_err(|e| format!("mcp_test_connection: {e}"))
}

/// Store (or, if blank, clear) the bearer token for a server. The token lives
/// in the HOST's `secrets/` dir (0600) — never in `config.toml` or the store,
/// so it can't ride along with anything the app shares, syncs, or gossips.
/// Blank-clears is the secret store's own contract, applied there.
#[tauri::command]
pub async fn mcp_set_token(
    state: State<'_, Arc<AppState>>,
    name: String,
    token: String,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Server name is required.".into());
    }
    mcp_client(&state)
        .mcp_set_token(name, &token)
        .await
        .map_err(|e| format!("mcp_set_token: {e}"))
}

/// Remove a server's stored token (a no-op if none is set, and `Ok` either
/// way: nothing to delete is not a failed delete).
#[tauri::command]
pub async fn mcp_clear_token(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    mcp_client(&state)
        .mcp_clear_token(name.trim())
        .await
        .map_err(|e| format!("mcp_clear_token: {e}"))
}
