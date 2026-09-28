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
//! This module was previously inlined in `sovereign-cli/src/project_cmd.rs`.
//! It lives here so the embedded daemon can mount it directly.

use std::future::Future;
use std::net::SocketAddr;

use crate::loopback_guard::LoopbackRouter;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use host_kit::mcp::{McpRequestContext, McpRequestHandler};
use serde_json::Value;
use tower_http::cors::CorsLayer;

use corpus_engine_notes::NoteStore;
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

// MCP allowlist + alias logic lives in
// [`sovereign_tools::mcp_surface`] so the daemon's mount and the
// standalone `sovereign serve` HTTP module agree on exactly the
// same surface. See that module for the full contract; this file
// just imports the helpers.
use sovereign_tools::mcp_surface::{
    is_mcp_exposed, negotiate_mcp_protocol_version, render_tools_list_gated, resolve_alias,
};

/// Phase 5 feature-root extension. When set, `tools/list` calls
/// [`render_tools_list_gated`] with this path so spec-gated tools
/// (`spec`, `drift`) only appear when `.sovereign/features/*/spec.md`
/// or `ARCHITECTURE.md` exists. `None` (the daemon's default) means
/// the gate is off and every exposed tool ships unconditionally —
/// preserving Phase 4 behaviour while we work out per-request gate
/// resolution for the embedded daemon path.
#[derive(Clone)]
pub struct FeatureRoot(pub Option<std::sync::Arc<std::path::PathBuf>>);

impl FeatureRoot {
    /// Construct from an optional path. The double-Arc layer lets us
    /// stuff this into an axum Extension cheaply (one shared Arc,
    /// not a new allocation per request).
    pub fn new(path: Option<std::path::PathBuf>) -> Self {
        Self(path.map(std::sync::Arc::new))
    }
}

/// The notifier behind `GET /mcp`. It moved to the host kit with the HTTP+SSE
/// framing (phase-b pb-code-server); this path is its historical one. The
/// watcher in `sovereign_tools::spec_watcher` calls
/// [`McpNotifier::notify_tools_list_changed`] from its `on_change` callback.
pub use host_kit::mcp::http::McpNotifier;

/// The daemon's own method dispatch behind the kit's HTTP framing: its tool
/// registry, call log, pattern matcher and surface filter.
struct DaemonMcp {
    tools: Arc<ToolRegistry>,
    logger: Arc<NoteStore>,
    session_id: Arc<String>,
    call_counter: Arc<AtomicU64>,
    feature_root: FeatureRoot,
    pattern_matcher: Arc<corpus_engine_notes::mining::patterns::ToolPatternMatcher>,
}

impl McpRequestHandler for DaemonMcp {
    fn handle(
        &self,
        req: JsonRpcRequest,
        ctx: &McpRequestContext,
    ) -> impl Future<Output = Option<JsonRpcResponse>> + Send {
        // Agent identity for the work atlas. Prefer the explicit header
        // the agent supplies (`X-Agent-Session`); fall back to the
        // per-MCP-connection `session_id` so we still get session
        // grouping for clients that don't set the header. Used only by
        // tools that read `ToolContext::agent_session_token`; everything
        // else ignores it.
        let agent_session_token = ctx
            .agent_session
            .clone()
            .unwrap_or_else(|| format!("conn:{}", self.session_id.as_str()));
        dispatch(
            req,
            Arc::clone(&self.tools),
            Arc::clone(&self.logger),
            Arc::clone(&self.session_id),
            Arc::clone(&self.call_counter),
            self.feature_root.clone(),
            Arc::clone(&self.pattern_matcher),
            agent_session_token,
        )
    }
}

/// Build the MCP router. Mounts `/mcp`, `/mcp/message`, and `/mcp/stats`
/// with shared per-session state (tool registry, note store, session id,
/// call counter, feature_root, notifier).
///
/// Phase 5: callers pass `feature_root = Some(dir)` to enable the
/// spec-presence gate. The standalone `sovereign serve` does this
/// with the cwd it was launched from. The embedded daemon currently
/// passes `None` so its `tools/list` matches Phase 4 behaviour; a
/// per-request gate via the project registry can wire in later.
///
/// Phase 5b: `notifier` is the broadcast surface for server-pushed
/// notifications (currently just `notifications/tools/list_changed`).
/// SSE handlers subscribe to it; producers (the spec watcher) push
/// to it. The router builder accepts an [`McpNotifier`] handle by
/// value so the caller can keep its own clone for triggering events.
/// If the caller has no producer, `McpNotifier::new()` is fine — the
/// channel is lazy and stays idle until something publishes.
pub fn mcp_router(
    tools: Arc<ToolRegistry>,
    logger: Arc<NoteStore>,
    session_id: String,
    feature_root: FeatureRoot,
    notifier: McpNotifier,
) -> Router {
    // Shared per-session call counter. Every REFLECT_HINT_INTERVAL tool
    // calls we append a brief reminder to write a session_reflection.
    let call_counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    // Phase 7.1: ToolPatternMatcher observes recent tool calls and
    // writes `source='observed'` notes for recognised patterns
    // (e.g. blast→build = "investigated impact, then acted"). One
    // instance per router so per-session cooldown state persists
    // across requests on the same session id. Fire-and-forget after
    // every successful tool dispatch.
    let pattern_matcher = Arc::new(
        corpus_engine_notes::mining::patterns::ToolPatternMatcher::new(Arc::clone(&logger)),
    );
    let handler = Arc::new(DaemonMcp {
        tools: Arc::clone(&tools),
        logger,
        session_id: Arc::new(session_id),
        call_counter,
        feature_root,
        pattern_matcher,
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

/// GET /mcp/stats — tool call counts since server start.
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
async fn dispatch(
    req: JsonRpcRequest,
    tools: Arc<ToolRegistry>,
    logger: Arc<NoteStore>,
    session_id: Arc<String>,
    call_counter: Arc<AtomicU64>,
    feature_root: FeatureRoot,
    pattern_matcher: Arc<corpus_engine_notes::mining::patterns::ToolPatternMatcher>,
    agent_session_token: String,
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
            // the SSE channel and refetch `tools/list` on the
            // server-pushed notification we now emit on spec
            // create/modify/remove.
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
            let descriptors = tools.descriptors();
            // Phase 5: feature_root.0 is `Some(Arc<PathBuf>)` for
            // spec-gated callers (standalone serve), `None` for the
            // daemon's pass-through. The cache amortises stat-storms.
            let tool_list = render_tools_list_gated(
                &descriptors,
                feature_root.0.as_deref().map(|p| p.as_path()),
            );
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
                pattern_matcher,
                agent_session_token,
            )
            .await
        }
        Some(McpMethod::Ping) => JsonRpcResponse::result(id, serde_json::json!({})),
        None => JsonRpcResponse::error(id, -32601, format!("method not found: {}", req.method)),
    };

    Some(response)
}

/// Execute a `tools/call` request. Logs the call to the tool_call_log
/// ring buffer for pattern analysis by `sovereign reflect`. Log
/// failures are silently ignored — they must never affect tool call
/// outcomes.
///
/// As of Phase 2 the tool response carries no trailing reminder text.
/// Stateful tools advertise their salient state through `Tool::signal`,
/// which the Runtime's ReasonWithTools preamble polls every turn.
async fn handle_tool_call(
    id: Value,
    params: Option<Value>,
    tools: Arc<ToolRegistry>,
    logger: Arc<NoteStore>,
    session_id: Arc<String>,
    call_counter: Arc<AtomicU64>,
    pattern_matcher: Arc<corpus_engine_notes::mining::patterns::ToolPatternMatcher>,
    agent_session_token: String,
) -> JsonRpcResponse {
    let Some(params) = params else {
        return JsonRpcResponse::error(id, -32602, "missing params");
    };
    let Some(raw_name) = params.get("name").and_then(|v| v.as_str()) else {
        return JsonRpcResponse::error(id, -32602, "missing 'name'");
    };
    // Alias rewrite: a client that cached the old MCP name (e.g.
    // `find_callers`) hits the same canonical handler as the new
    // name (`callers`). Telemetry (`record_call`) is keyed off the
    // canonical name so call counts aggregate across both spellings.
    let canonical = resolve_alias(raw_name).to_string();
    let name = canonical;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    // The ToolRegistry half (validate, write audit, ToolContext, execute,
    // StepOutput mapping) is sovereign-contracts' `mcp_host`, which the code
    // server runs too; the log, the pattern matcher and the counter are
    // this daemon's.
    let Some(outcome) = sovereign_contracts::mcp_host::call_registry_tool(
        &tools,
        &name,
        &arguments,
        &session_id,
        agent_session_token,
        is_mcp_exposed,
    )
    .await
    else {
        return JsonRpcResponse::error(id, -32601, format!("tool not found: {raw_name}"));
    };

    // Log outcome to ring buffer. Fire-and-forget — a logging failure must
    // never affect the tool call result. A call that executed no tool (not
    // registered, arguments refused) is not logged.
    if let Some(tag) = sovereign_contracts::mcp_host::call_log_tag(&outcome) {
        let _ = logger.log_tool_call(&session_id, &name, tag).await;

        // Phase 7.1: run the pattern matcher against the freshly-logged
        // call. Fire-and-forget on a tokio task so a slow DB write
        // (writing an `observed`-source note) doesn't lengthen the tool
        // response. The matcher's per-session state lives on the Arc'd
        // matcher; cooldowns persist across requests on the same
        // session id.
        let matcher_for_task = Arc::clone(&pattern_matcher);
        let session_for_task = Arc::clone(&session_id);
        tokio::spawn(async move {
            matcher_for_task
                .observe_and_record(session_for_task.as_str(), None)
                .await;
        });

        // The session call counter is kept for telemetry / rate-limit
        // decisions even though the periodic reflection nudge was removed
        // in Phase 2. Tools now surface their salient state via
        // `Tool::signal()` which the ReasonWithTools preamble polls every
        // turn — the 10-call text nudge ("Consider calling
        // session_reflection…") is obsolete.
        let _ = call_counter.fetch_add(1, Ordering::Relaxed);
    }

    JsonRpcResponse::result(id, outcome.into_call_result())
}
