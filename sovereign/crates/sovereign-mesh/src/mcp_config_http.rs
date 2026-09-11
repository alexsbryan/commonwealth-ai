// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's OWN external-MCP configuration — what is declared, what is
//! actually mounted, and the bearer secrets (sv-surface D8, second half).
//!
//! `commands/mcp_servers.rs` joined `SetupConfig::load()` with the DESKTOP's
//! `McpServerManager`; an attached desktop assembles no tool registry, so
//! that manager describes connections nobody uses (campaign X-list). These
//! routes report the daemon's registry instead.
//!
//! Loopback-only, `reading_http`'s posture: `PUT …/token` writes a bearer
//! secret to `~/.svrnmesh/secrets/mcp/` at 0600.
//!
//! NOT served, named rather than guessed (ARCH §18.3):
//! - `connected` / `error` — the daemon keeps no `McpServerManager`, so they
//!   have no source here; [`McpServerView::live_tool_count`] is an
//!   observation served in their place and [`McpMountStatus::reason`] carries
//!   the absence in words.
//! - `mcp_add_server` / `mcp_remove_server` — both `SetupConfig::save()`,
//!   which has zero call sites in this crate; who owns `config.toml` is a
//!   decision, not a side effect of this rung.
//!
//! Named imprecision: ids are `mcp_<server>_<tool>` and both halves may carry
//! `_`, so the prefix count is exact for every server name that is not a
//! `_`-extended prefix of another.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_contracts::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
use sovereign_tools::mcp::auth::{secret_env_var, McpAuth};
use sovereign_tools::mcp::secret_store;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::json_error;
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

// ─── Wire types ────────────────────────────────────────────────

/// One configured MCP server, joined with what the daemon's tool registry
/// actually holds for it.
///
/// The first seven fields are the desktop's `McpServerView` field-for-
/// field. `connected` / `tool_count` / `error` are deliberately absent —
/// see this module's header — and [`Self::live_tool_count`] replaces them
/// with the fact the daemon can actually observe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub url: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub bearer: bool,
    /// Env var the bearer token is read from — the headless / CI
    /// override. `None` for no-auth servers.
    pub token_env: Option<String>,
    /// Whether a token is currently stored in the secret file for this
    /// server (the primary path).
    pub has_token: bool,
    /// Tools in the daemon's live registry whose id carries this server's
    /// `mcp_<name>_` prefix. `0` on a daemon with no `/mcp` mount at all —
    /// which [`McpMountStatus`] distinguishes from "mounted, zero tools".
    pub live_tool_count: usize,
}

/// Whether the daemon has a tool mount to count against, and — when it
/// does not — why.
///
/// Mirrors [`crate::daemon_services::McpSurface`], which exists for
/// exactly this reason: "this host serves no tools" and "`notes.db` would
/// not open" are different operational facts with different fixes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpMountStatus {
    /// `true` when a tool registry was available to fold the counts over.
    /// When `false` every `live_tool_count` above is `0` because there was
    /// nothing to count, NOT because the servers registered nothing.
    pub mounted: bool,
    /// Total tools in the daemon's registry, MCP and native alike — the
    /// denominator for the per-server counts.
    pub total_tools: usize,
    /// Why connect status and connect errors are not in this payload, in
    /// words, on every response. Always present: an absence a caller has
    /// to infer from a missing key is indistinguishable from an old host
    /// (ARCH §18.3).
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServersResponse {
    pub servers: Vec<McpServerView>,
    pub mount: McpMountStatus,
}

/// `POST /v1/mcp/servers/test` — probe a server without persisting it.
///
/// `token` is the one the operator just typed and has not saved, so
/// "Test" reflects the form; absent, the stored/env secret for an
/// already-saved server is used.
#[derive(Debug, Deserialize)]
pub struct TestConnectionRequest {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub bearer: bool,
    #[serde(default)]
    pub token: Option<String>,
}

/// How many tools the probed server offered. A reachable server with zero
/// tools is a success answering `0`, not an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConnectionResponse {
    pub tool_count: usize,
}

/// `PUT /v1/mcp/servers/{name}/token` — store a bearer token. A blank
/// token CLEARS it, which is `secret_store::write_token`'s own contract
/// and not a second decider here.
#[derive(Debug, Deserialize)]
pub struct SetTokenRequest {
    pub token: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The MCP-configuration router. Mounted unconditionally on serving
/// daemons: a daemon with no `/mcp` mount answers a 200 whose
/// [`McpMountStatus::mounted`] is `false`, because "no tool mount" is an
/// answer to this question rather than a failure to answer it.
pub fn mcp_config_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/mcp/servers", get(list_servers))
        .route("/v1/mcp/servers/test", post(test_connection))
        .route(
            "/v1/mcp/servers/{name}/token",
            put(set_token).delete(clear_token),
        )
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/mcp/servers` — the daemon's configured servers, annotated
/// with what its live registry holds for each.
///
/// The config read is the daemon's IN-MEMORY `SetupConfig`, not a fresh
/// `SetupConfig::load()`. That is deliberate and it is the more truthful
/// of the two: the in-memory copy is what this daemon actually loaded
/// servers from, and it advances on `POST /v1/admin/reload`. A file
/// edited since boot describes a daemon that does not exist yet, which is
/// precisely the state `reload` exists to end.
///
/// An empty list is the right answer for an operator who configured none,
/// and must never become a 404.
async fn list_servers(_: LocalOnly, Extension(daemon): Extension<Arc<EmbeddedDaemon>>) -> Response {
    let configured = daemon.configured_mcp_servers().await;

    // The live fold: tool ids in the daemon's own registry.
    let (mounted, tool_ids) = match daemon.mcp_tool_ids() {
        Some(ids) => (true, ids),
        None => (false, Vec::new()),
    };

    let servers: Vec<McpServerView> = configured
        .into_iter()
        .map(|s| view_for(&s, &tool_ids))
        .collect();

    tracing::debug!(
        configured = servers.len(),
        mounted,
        total_tools = tool_ids.len(),
        "mcp_config_http: server list served",
    );
    Json(McpServersResponse {
        servers,
        mount: McpMountStatus {
            mounted,
            total_tools: tool_ids.len(),
            reason: MOUNT_REASON.to_string(),
        },
    })
    .into_response()
}

/// POST `/v1/mcp/servers/test` — connect to an HTTP MCP server and count
/// its tools, persisting nothing.
///
/// A failure is a 502, not a 500: the fault is the remote server's or the
/// URL's, and the caller can act on that (fix the address, fix the token)
/// where a 500 tells them only that something broke here.
async fn test_connection(
    _: LocalOnly,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<TestConnectionRequest>,
) -> Response {
    if body.url.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Server URL is required.");
    }
    // Prefer the token the caller just typed (not yet saved); otherwise
    // fall back to the stored / env secret for an already-saved server.
    let auth = if !body.bearer {
        McpAuth::None
    } else if let Some(t) = body.token.filter(|t| !t.trim().is_empty()) {
        McpAuth::BearerToken(t.trim().to_string())
    } else {
        McpAuth::resolve(&body.name, &McpAuthConfig::Bearer)
    };
    match sovereign_tools::mcp::connect_http_mcp_server(&body.url, auth, &body.name).await {
        Ok(tools) => {
            tracing::debug!(server = %body.name, tool_count = tools.len(),
                "mcp_config_http: probe succeeded");
            Json(TestConnectionResponse {
                tool_count: tools.len(),
            })
            .into_response()
        }
        Err(e) => {
            tracing::debug!(server = %body.name, error = %e, "mcp_config_http: probe failed");
            json_error(StatusCode::BAD_GATEWAY, &e.to_string())
        }
    }
}

/// PUT `/v1/mcp/servers/{name}/token` — store (or, blank, clear) the
/// bearer token.
///
/// The token lands in `~/.svrnmesh/secrets/mcp/` at 0600 — never in
/// `config.toml` or any store, so it cannot ride along with anything the
/// host shares, syncs or gossips. That directory is resolved from the
/// rebrand root, which is the same directory the daemon's own MCP loader
/// reads at boot: writing here is writing where it will be read.
async fn set_token(
    _: LocalOnly,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(name): Path<String>,
    Json(body): Json<SetTokenRequest>,
) -> Response {
    let name = name.trim();
    if name.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Server name is required.");
    }
    match secret_store::write_token(name, &body.token) {
        Ok(()) => {
            // The token itself is never logged; whether one is now set is.
            tracing::info!(server = %name, set = !body.token.trim().is_empty(),
                "mcp_config_http: bearer secret updated");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("store token: {e}"),
        ),
    }
}

/// DELETE `/v1/mcp/servers/{name}/token` — clear a stored token. A no-op
/// when none is set, and 204 either way: "there was nothing to delete" is
/// not a failure of a delete.
async fn clear_token(
    _: LocalOnly,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(name): Path<String>,
) -> Response {
    match secret_store::delete_token(name.trim()) {
        Ok(()) => {
            tracing::info!(server = %name.trim(), "mcp_config_http: bearer secret cleared");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("clear token: {e}"),
        ),
    }
}

// ─── Helpers ───────────────────────────────────────────────────

/// Why `connected` and `error` are not on the wire. Stated once, sent on
/// every response.
const MOUNT_REASON: &str = "live connect status and connect errors are not available from this \
     daemon: the boot-time McpServerManager is dropped after it registers tools \
     (sovereign-runtime-recipe), so the only observable per-server fact here is how many of its \
     tools are in the live registry. `live_tool_count` is that observation; no connect state is \
     inferred from it.";

/// The URL a transport points at. A stdio server is rendered as
/// `stdio:<command>` — the desktop's spelling, kept because the pane
/// shows it verbatim and `svrnmesh` does not supervise subprocesses
/// anyway, so this arm is display-only.
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

/// Tools registered under `mcp_<name>_`. See the header for the one case
/// this over-counts and why the fix is not a second id encoding.
fn live_tool_count(name: &str, tool_ids: &[String]) -> usize {
    let prefix = format!("mcp_{name}_");
    tool_ids.iter().filter(|id| id.starts_with(&prefix)).count()
}

fn view_for(s: &McpServerConfig, tool_ids: &[String]) -> McpServerView {
    let bearer = is_bearer(&s.transport);
    McpServerView {
        name: s.name.clone(),
        url: http_url(&s.transport),
        description: s.description.clone(),
        enabled: s.enabled,
        bearer,
        token_env: bearer.then(|| secret_env_var(&s.name)),
        has_token: bearer && secret_store::has_token(&s.name),
        live_tool_count: live_tool_count(&s.name, tool_ids),
    }
}
