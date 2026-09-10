// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon serves its OWN external-MCP configuration — what is
//! declared, what is actually mounted, and the bearer secrets
//! (sv-surface D8, second half).
//!
//! # The twin, and which half of it is real
//!
//! `commands/mcp_servers.rs` reads TWO things and joins them:
//! `SetupConfig::load()` (the canonical `config.toml`, the same
//! `[[mcp_servers]]` array `svrn chat` and `svrn serve` read) and
//! `state.mcp_servers` — the DESKTOP's own `McpServerManager`, holding
//! statuses captured when the desktop last connected those servers into
//! its own tool registry.
//!
//! The campaign's X-list settles the second half up front:
//! *"state.mcp_servers live status is a twin to delete, not proxy"*. An
//! attached desktop assembles no tool registry, so its manager describes
//! connections nobody is using. The daemon's registry is the one an
//! answer actually plans against, and that is what these routes report.
//!
//! # What "live" can honestly mean here, and what it cannot
//!
//! The daemon does not KEEP an `McpServerManager`.
//! `sovereign-runtime-recipe` builds one at boot, prints its per-server
//! banner and drops it, and says why in its own comment: keeping it would
//! need a keep-alive in the `ToolBundle` seam's return type, which is a
//! change to the seam rather than a use of it. So the two fields the
//! desktop renders from that manager — `connected: Option<bool>` and
//! `error: Option<String>` — have no source on this host.
//!
//! They are therefore NOT served, rather than served as a plausible
//! guess. What IS served is [`McpServerView::live_tool_count`]: how many
//! tools bearing this server's `mcp_<name>_` prefix are in the registry
//! behind the daemon's `/mcp` mount right now. That is an observation,
//! not an inference. Reporting `connected: live_tool_count > 0` would
//! have been the substitution §18.3 forbids — a server that legitimately
//! exports zero tools is connected and would read as failed, and a server
//! that connected and was later removed from the registry would read as
//! never-configured.
//!
//! [`McpMountStatus::reason`] carries that absence in words on every
//! response, so a caller rendering a settings pane can say "connect
//! errors are not available from this host" instead of drawing a green
//! dot it invented. The structural fix is an `McpServerManager` on
//! [`crate::daemon_services::McpMount`], and it is a seam change in
//! `sovereign-tools-base` + `sovereign-runtime-recipe` — a rung, not a
//! door.
//!
//! Named imprecision in the prefix fold: tool ids are
//! `mcp_<server>_<tool>` and both halves may contain `_`, so a server
//! named `a` and a server named `a_b` cannot be told apart by prefix for
//! a tool called `b_c`. The count is exact for every server name that is
//! not a `_`-extended prefix of another. The structural fix is the
//! manager above, which knows which tools it registered; a second id
//! encoding invented here would be the §10.6 smell.
//!
//! # The two config WRITES do not cross, and this is why
//!
//! `mcp_add_server` and `mcp_remove_server` mutate `config.toml` through
//! `SetupConfig::save()`. **This daemon owns no config-write path** —
//! measured, not assumed: `SetupConfig::save()` has zero call sites in
//! `sovereign-mesh`, and the writers are all CLI-side (`setup_cmd/finish`,
//! `setup_cmd/terminal`, `setup_cmd/fim`, `model_cmd`). `admin_http`
//! offers `POST /v1/admin/reload`, which RE-READS the file; it does not
//! write it. Adding the first HTTP config write is a decision about who
//! owns `config.toml` — one writer per data root is what the run lock
//! buys — and it belongs to whoever takes it deliberately, not to this
//! rung as a side effect. The two commands stay app-local with that
//! reason recorded; a caller wanting them over the wire needs
//! `POST /v1/admin/config` first, and then `POST /v1/admin/reload` to
//! make the daemon act on it.
//!
//! # Loopback only
//!
//! `reading_http`'s posture. `PUT .../token` writes a bearer secret to
//! `~/.svrnmesh/secrets/mcp/` at 0600; nothing about this family is
//! peer-facing.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_contracts::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
use sovereign_tools::mcp::auth::{secret_env_var, McpAuth};
use sovereign_tools::mcp::secret_store;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

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

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
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
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
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
async fn list_servers(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<TestConnectionRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    if body.url.trim().is_empty() {
        return error_body(StatusCode::BAD_REQUEST, "Server URL is required.");
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
            error_body(StatusCode::BAD_GATEWAY, &e.to_string())
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(name): Path<String>,
    Json(body): Json<SetTokenRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let name = name.trim();
    if name.is_empty() {
        return error_body(StatusCode::BAD_REQUEST, "Server name is required.");
    }
    match secret_store::write_token(name, &body.token) {
        Ok(()) => {
            // The token itself is never logged; whether one is now set is.
            tracing::info!(server = %name, set = !body.token.trim().is_empty(),
                "mcp_config_http: bearer secret updated");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("store token: {e}"),
        ),
    }
}

/// DELETE `/v1/mcp/servers/{name}/token` — clear a stored token. A no-op
/// when none is set, and 204 either way: "there was nothing to delete" is
/// not a failure of a delete.
async fn clear_token(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(_daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    match secret_store::delete_token(name.trim()) {
        Ok(()) => {
            tracing::info!(server = %name.trim(), "mcp_config_http: bearer secret cleared");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => error_body(
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

fn error_body(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}
