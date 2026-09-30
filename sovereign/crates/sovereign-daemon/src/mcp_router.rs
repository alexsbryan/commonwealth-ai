// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP (Model Context Protocol) HTTP/SSE router.
//!
//! Mounts at `/mcp`, `/mcp/message`, and `/mcp/stats`. Local-only —
//! requests from non-loopback addresses receive `403 Forbidden`. Used
//! by [`EmbeddedDaemon`](crate::daemon::EmbeddedDaemon) when configured
//! via [`with_mcp`](crate::daemon::EmbeddedDaemon::with_mcp) so that
//! `localhost:9741` serves both the OpenAI-compatible `/v1` surface
//! and the tool-use MCP surface on a single port.
//!
//! `:9741/mcp` is the one MCP address (phase-b-33). It serves svrn's own
//! tools and, when a distribution composed the code program into this
//! process, code's tools beside them, each program's calls running and
//! logging through its own host (pb-code-daemon-exit). svrn alone answers a
//! code tool with a pointer to `svrn code mcp`.

use std::future::Future;
use std::net::SocketAddr;

use crate::loopback_guard::LoopbackRouter;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use host_kit::mcp::{
    McpCallLog, McpMountedTools, McpRequestContext, McpRequestHandler, ToolOutcome,
};
use serde_json::Value;
use tower_http::cors::CorsLayer;

use sovereign_contracts::notes::AgentNotes;
use sovereign_core::registry::ToolRegistry;

// ─── JSON-RPC 2.0 envelope ────────────────────────────────────
//
// Imported, not re-declared. The envelope is a wire contract, so it lives at
// layer 0 in `oicp-types` (reached here through `sovereign_core`'s re-export,
// per ARCH §8.3) and corpus-mcp's stdio server imports the same one.
//
// This router's `Option<Value>` id is the shape that won the adjudication; the
// visible change here is that the error object now carries the spec's optional
// `data` member, which serializes to nothing while it is `None`.
use sovereign_core::oicp::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use sovereign_core::oicp::mcp::McpMethod;

// svrn's exposure list; code's is code's (phase-b pb-code-freshness) and
// applies inside code's mounted host.
use sovereign_tools::mcp_surface::{is_mcp_exposed, negotiate_mcp_protocol_version};

/// The notifier behind `GET /mcp`. It moved to the host kit with the HTTP+SSE
/// framing (phase-b pb-code-server); this path is its historical one.
pub use host_kit::mcp::http::McpNotifier;

/// svrn's call log: the kit's call-log port over svrn's own store
/// (pb-notes-memory), so svrn's calls land beside its other memory and
/// code's `svrn reflect` reads only code's. Fire-and-forget: a log failure
/// never touches the answer.
struct SvrnCallLog {
    store: Arc<dyn AgentNotes>,
    session_id: Arc<String>,
}

impl McpCallLog for SvrnCallLog {
    fn record(&self, tool: &str, outcome: &ToolOutcome, _ctx: &McpRequestContext) {
        let Some(tag) = sovereign_contracts::mcp_host::call_log_tag(outcome) else {
            tracing::debug!(tool, "mcp: no tool executed, nothing logged");
            return;
        };
        let (store, session, tool) = (
            Arc::clone(&self.store),
            Arc::clone(&self.session_id),
            tool.to_string(),
        );
        tokio::spawn(async move {
            if let Err(e) = store.log_tool_call(&session, &tool, tag).await {
                tracing::debug!(tool = %tool, error = %e, "mcp: svrn's call-log write failed");
            }
        });
    }
}

/// The daemon's own method dispatch behind the kit's HTTP framing: svrn's
/// registry, call log and surface filter, and code's mounted tools.
struct DaemonMcp {
    tools: Arc<ToolRegistry>,
    logger: Arc<SvrnCallLog>,
    session_id: Arc<String>,
    call_counter: Arc<AtomicU64>,
    code: Option<Arc<dyn McpMountedTools>>,
}

impl McpRequestHandler for DaemonMcp {
    fn handle(
        &self,
        req: JsonRpcRequest,
        ctx: &McpRequestContext,
    ) -> impl Future<Output = Option<JsonRpcResponse>> + Send {
        let agent_session_token = sovereign_contracts::mcp_host::agent_session_token(
            ctx.agent_session.clone(),
            &self.session_id,
        );
        dispatch(
            req,
            Arc::clone(&self.tools),
            Arc::clone(&self.logger),
            Arc::clone(&self.session_id),
            Arc::clone(&self.call_counter),
            self.code.clone(),
            agent_session_token,
            ctx.clone(),
        )
    }
}

/// Build the MCP router. Mounts `/mcp`, `/mcp/message`, and `/mcp/stats`
/// with shared per-session state (svrn's tool registry, svrn's store for its
/// call log, session id, call counter, notifier) and code's mounted tools,
/// if any.
///
/// Phase 5b: `notifier` is the broadcast surface for server-pushed
/// notifications (currently just `notifications/tools/list_changed`).
/// SSE handlers subscribe to it; producers push to it. If the caller has no
/// producer, `McpNotifier::new()` is fine — the channel is lazy and stays
/// idle until something publishes.
pub fn mcp_router(
    tools: Arc<ToolRegistry>,
    notes: Arc<dyn AgentNotes>,
    session_id: String,
    code: Option<Arc<dyn McpMountedTools>>,
    notifier: McpNotifier,
) -> Router {
    // Shared per-session call counter.
    let call_counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let session_id = Arc::new(session_id);
    let handler = Arc::new(DaemonMcp {
        tools: Arc::clone(&tools),
        logger: Arc::new(SvrnCallLog {
            store: notes,
            session_id: Arc::clone(&session_id),
        }),
        session_id,
        call_counter,
        code,
    });
    // `/mcp` and `/mcp/message` are the kit's framing over this daemon's
    // dispatch; `/mcp/stats` is the daemon's own.
    host_kit::mcp::http::routes(handler, notifier)
        .route("/mcp/stats", axum::routing::get(mcp_stats))
        // Router-level loopback guard — catches any future MCP route
        // added here even if the author forgets the per-handler
        // `is_localhost` check. The per-handler check stays for
        // defense in depth.
        .localhost_only()
        .layer(Extension(tools))
        .layer(CorsLayer::permissive())
}

fn is_localhost(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// GET /mcp/stats — svrn's tool call counts since server start.
async fn mcp_stats(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tools): Extension<Arc<ToolRegistry>>,
) -> impl IntoResponse {
    if !is_localhost(&peer) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "local-only"})),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        Json(sovereign_contracts::mcp_host::call_stats(&tools)),
    )
        .into_response()
}

/// Dispatch a JSON-RPC request to the appropriate handler.
///
/// Returns `Some(JsonRpcResponse)` for calls (requests with an id) and
/// `None` for notifications (no id, no reply per JSON-RPC spec).
#[allow(clippy::too_many_arguments)]
async fn dispatch(
    req: JsonRpcRequest,
    tools: Arc<ToolRegistry>,
    logger: Arc<SvrnCallLog>,
    session_id: Arc<String>,
    call_counter: Arc<AtomicU64>,
    code: Option<Arc<dyn McpMountedTools>>,
    agent_session_token: String,
    ctx: McpRequestContext,
) -> Option<JsonRpcResponse> {
    // Notifications: no id → no response. We still want to accept the
    // method (e.g. `notifications/initialized`) so the client doesn't see
    // an error. Return None so the handler sends 202 Accepted.
    let Some(id) = req.id else {
        tracing::debug!(method = %req.method, "mcp: notification received");
        return None;
    };

    let response = match McpMethod::parse(&req.method) {
        Some(McpMethod::Initialize) => {
            // Phase 5b: advertise `tools.listChanged: true` so MCP
            // clients (Claude Code, Cursor, opencode) subscribe to
            // the SSE channel and refetch `tools/list` on a
            // server-pushed notification.
            let result = serde_json::json!({
                "protocolVersion": negotiate_mcp_protocol_version(req.params.as_ref()),
                "capabilities": {
                    "tools": { "listChanged": true }
                },
                "serverInfo": {
                    "name": "sovereign-code",
                    "version": env!("CARGO_PKG_VERSION")
                }
            });
            JsonRpcResponse::result(id, result)
        }
        Some(McpMethod::ToolsList) => {
            let mut tool_list = sovereign_contracts::mcp_host::render_tool_entries(
                &tools.descriptors(),
                is_mcp_exposed,
            );
            match &code {
                Some(code) => {
                    if let Value::Array(entries) = code.list() {
                        tool_list.extend(entries);
                    }
                }
                None => tracing::debug!("mcp: tools/list is svrn's alone; no code program here"),
            }
            JsonRpcResponse::result(id, serde_json::json!({ "tools": tool_list }))
        }
        Some(McpMethod::ToolsCall) => {
            handle_tool_call(
                id,
                req.params,
                tools,
                logger,
                session_id,
                call_counter,
                code,
                agent_session_token,
                &ctx,
            )
            .await
        }
        Some(McpMethod::Ping) => JsonRpcResponse::result(id, serde_json::json!({})),
        None => JsonRpcResponse::error(id, -32601, format!("method not found: {}", req.method)),
    };

    Some(response)
}

/// Execute a `tools/call` request: svrn's tool through svrn's registry and
/// call log, else code's through code's mounted host (which logs its own
/// calls), else a -32601 that, with no code program here, names the server
/// that has code's tools. Log failures never affect a call's outcome.
///
/// As of Phase 2 the tool response carries no trailing reminder text.
/// Stateful tools advertise their salient state through `Tool::signal`,
/// which the Runtime's ReasonWithTools preamble polls every turn.
#[allow(clippy::too_many_arguments)]
async fn handle_tool_call(
    id: Value,
    params: Option<Value>,
    tools: Arc<ToolRegistry>,
    logger: Arc<SvrnCallLog>,
    session_id: Arc<String>,
    call_counter: Arc<AtomicU64>,
    code: Option<Arc<dyn McpMountedTools>>,
    agent_session_token: String,
    ctx: &McpRequestContext,
) -> JsonRpcResponse {
    let Some(params) = params else {
        return JsonRpcResponse::error(id, -32602, "missing params");
    };
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return JsonRpcResponse::error(id, -32602, "missing 'name'");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    // The ToolRegistry half (validate, write audit, ToolContext, execute,
    // StepOutput mapping) is sovereign-contracts' `mcp_host`, which the code
    // server runs too; the log and the counter are this daemon's.
    let Some(outcome) = sovereign_contracts::mcp_host::call_registry_tool(
        &tools,
        name,
        &arguments,
        &session_id,
        agent_session_token,
        is_mcp_exposed,
    )
    .await
    else {
        return match code {
            Some(code) => match code.call(name, &arguments, ctx).await {
                Some(outcome) => {
                    tracing::debug!(tool = name, "mcp: code's tool ran");
                    JsonRpcResponse::result(id, outcome.into_call_result())
                }
                None => {
                    tracing::debug!(tool = name, "mcp: neither svrn nor code has this tool");
                    JsonRpcResponse::error(id, -32601, format!("tool not found: {name}"))
                }
            },
            None => {
                tracing::debug!(
                    tool = name,
                    "mcp: not svrn's tool, and no code program here"
                );
                JsonRpcResponse::error(
                    id,
                    -32601,
                    format!(
                        "tool not found: {name} — svrn serves no code tool; code intelligence, \
                         notes, the work atlas and the solver are served by `{}`",
                        crate::hosted_code::CODE_SERVER
                    ),
                )
            }
        };
    };

    // Log the outcome through svrn's call log. A call that executed no tool
    // (not registered, arguments refused) is not logged or counted.
    logger.record(name, &outcome, ctx);
    if sovereign_contracts::mcp_host::call_log_tag(&outcome).is_some() {
        // The session call counter is kept for telemetry / rate-limit
        // decisions.
        let _ = call_counter.fetch_add(1, Ordering::Relaxed);
    }

    JsonRpcResponse::result(id, outcome.into_call_result())
}
