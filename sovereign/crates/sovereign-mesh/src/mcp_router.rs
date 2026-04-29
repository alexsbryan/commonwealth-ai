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

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::stream::{self, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_http::cors::CorsLayer;

use corpus_engine::NoteStore;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::types::{Effect, StepOutput, ToolContext};

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    #[serde(default)]
    pub jsonrpc: String,
    /// Optional — JSON-RPC notifications (e.g. `notifications/initialized`)
    /// omit the id and must not receive a response.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    fn ok(id: Value, value: Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(value), error: None }
    }
    fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.into() }),
        }
    }
}

fn call_tool_text(text: impl Into<String>, is_error: bool) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": is_error,
    })
}

// MCP allowlist + alias logic lives in
// [`sovereign_tools::mcp_surface`] so the daemon's mount and the
// standalone `sovereign serve` HTTP module agree on exactly the
// same surface. See that module for the full contract; this file
// just imports the helpers.
use sovereign_tools::mcp_surface::{is_mcp_exposed, render_tools_list_gated, resolve_alias};

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

/// Build the MCP router. Mounts `/mcp`, `/mcp/message`, and `/mcp/stats`
/// with shared per-session state (tool registry, note store, session id,
/// call counter, feature_root).
///
/// Phase 5: callers pass `feature_root = Some(dir)` to enable the
/// spec-presence gate. The standalone `sovereign serve` does this
/// with the cwd it was launched from. The embedded daemon currently
/// passes `None` so its `tools/list` matches Phase 4 behaviour; a
/// per-request gate via the project registry can wire in later.
pub fn mcp_router(
    tools: Arc<ToolRegistry>,
    logger: Arc<NoteStore>,
    session_id: String,
    feature_root: FeatureRoot,
) -> Router {
    // Shared per-session call counter. Every REFLECT_HINT_INTERVAL tool
    // calls we append a brief reminder to write a session_reflection.
    let call_counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    Router::new()
        // Both URLs accept the full JSON-RPC dispatch.
        // `POST /mcp` is the modern (2025-03-26 Streamable HTTP) entry point.
        // `POST /mcp/message` is kept for backward compatibility with clients
        // that followed the 2024-11-05 HTTP+SSE transport where the message
        // endpoint was a separate URL.
        .route("/mcp", post(mcp_handle).get(mcp_sse))
        .route("/mcp/message", post(mcp_handle))
        .route("/mcp/stats", axum::routing::get(mcp_stats))
        // Router-level loopback guard — catches any future MCP route
        // added here even if the author forgets the per-handler
        // `is_localhost` check. The per-handler check stays for
        // defense in depth.
        .layer(axum::middleware::from_fn(crate::loopback_guard::loopback_only))
        .layer(Extension(tools))
        .layer(Extension(logger))
        .layer(Extension(Arc::new(session_id)))
        .layer(Extension(call_counter))
        .layer(Extension(feature_root))
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
    let counts = tools.call_counts();
    let total: u64 = counts.iter().map(|(_, n)| n).sum();
    let tools_json: Vec<serde_json::Value> = counts
        .into_iter()
        .map(|(name, count)| serde_json::json!({ "tool": name, "calls": count }))
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "total_calls": total, "tools": tools_json })),
    )
        .into_response()
}

/// Emit the `endpoint` event required by the 2024-11-05 HTTP+SSE transport.
/// Clients open this stream first, wait for the endpoint URL, then POST
/// JSON-RPC messages to it. We point them back at `/mcp` itself so both
/// transports converge on the same handler.
async fn mcp_sse(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    if !is_localhost(&peer) {
        return Err(StatusCode::FORBIDDEN);
    }
    // Emit exactly one `endpoint` event, then hold the connection open
    // with keepalive so spec-compliant clients stay subscribed.
    let endpoint_event = stream::once(async {
        Ok::<_, Infallible>(Event::default().event("endpoint").data("/mcp"))
    });
    let forever = stream::pending::<Result<Event, Infallible>>();
    Ok(Sse::new(endpoint_event.chain(forever)).keep_alive(KeepAlive::default()))
}

/// Single JSON-RPC handler for both `/mcp` and `/mcp/message`.
/// Notifications (requests without an `id`) receive an empty 204 response
/// — per JSON-RPC 2.0, notifications have no reply.
async fn mcp_handle(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tools): Extension<Arc<ToolRegistry>>,
    Extension(logger): Extension<Arc<NoteStore>>,
    Extension(session_id): Extension<Arc<String>>,
    Extension(call_counter): Extension<Arc<AtomicU64>>,
    Extension(feature_root): Extension<FeatureRoot>,
    Json(req): Json<JsonRpcRequest>,
) -> axum::response::Response {
    if !is_localhost(&peer) {
        let id = req.id.clone().unwrap_or(Value::Null);
        return (
            StatusCode::FORBIDDEN,
            Json(JsonRpcResponse::err(id, -32001, "MCP is local-only")),
        )
            .into_response();
    }

    match dispatch(req, tools, logger, session_id, call_counter, feature_root).await {
        Some(response) => (StatusCode::OK, Json(response)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
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
) -> Option<JsonRpcResponse> {
    // Notifications: no id → no response. We still want to accept the
    // method (e.g. `notifications/initialized`) so the client doesn't see
    // an error. Return None so the handler sends 204 No Content.
    let Some(id) = req.id else {
        tracing::debug!(method = %req.method, "mcp: notification received");
        return None;
    };

    let response = match req.method.as_str() {
        "initialize" => {
            let result = serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "sovereign-code",
                    "version": env!("CARGO_PKG_VERSION")
                }
            });
            JsonRpcResponse::ok(id, result)
        }
        "tools/list" => {
            let descriptors = tools.descriptors();
            // Phase 5: feature_root.0 is `Some(Arc<PathBuf>)` for
            // spec-gated callers (standalone serve), `None` for the
            // daemon's pass-through. The cache amortises stat-storms.
            let tool_list = render_tools_list_gated(
                &descriptors,
                feature_root.0.as_deref().map(|p| p.as_path()),
            );
            JsonRpcResponse::ok(id, serde_json::json!({ "tools": tool_list }))
        }
        "tools/call" => handle_tool_call(id, req.params, tools, logger, session_id, call_counter).await,
        "ping" => JsonRpcResponse::ok(id, serde_json::json!({})),
        other => JsonRpcResponse::err(id, -32601, format!("method not found: {other}")),
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
) -> JsonRpcResponse {
    let Some(params) = params else {
        return JsonRpcResponse::err(id, -32602, "missing params");
    };
    let Some(raw_name) = params.get("name").and_then(|v| v.as_str()) else {
        return JsonRpcResponse::err(id, -32602, "missing 'name'");
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

    if !is_mcp_exposed(&name) {
        return JsonRpcResponse::err(id, -32601, format!("tool not found: {raw_name}"));
    }

    let tool = match tools.get(&name) {
        Ok(t) => t,
        Err(_) => {
            return JsonRpcResponse::ok(
                id,
                call_tool_text(
                    format!("`{name}` not registered. Run `sovereign project init` first."),
                    false,
                ),
            );
        }
    };

    if let Err(e) = tool.validate(&arguments) {
        return JsonRpcResponse::ok(id, call_tool_text(e.to_string(), true));
    }

    // Phase 1.5 audit gate: MCP is stdio/HTTP non-interactive, so the
    // executor's `ApprovalChannel::request_approval` path (which blocks
    // on human input) can't fire here. Instead we AUDIT every write-
    // effectful MCP call — tracing::warn!, plus a dedicated outcome
    // tag in the ring buffer — so an operator running `sovereign
    // reflect` sees every unapproved write after the fact. A future
    // interactive-MCP protocol extension can upgrade this to a hard
    // block without changing the surrounding structure.
    //
    // See ARCH_PRINCIPLES.md §7 (structural invariants) and §9
    // (glassbox). The parity gate to the executor's StepKind::Tool
    // path closes once MCP has an approval protocol; until then
    // visibility is the achievable half.
    let descriptor = tool.descriptor();
    let is_write_effectful = descriptor.effect != Effect::Read;
    if is_write_effectful {
        tracing::warn!(
            tool_id = %name,
            effect = ?descriptor.effect,
            idempotency = ?descriptor.idempotency,
            session_id = %session_id,
            "mcp: write-effectful tool invoked without approval gate \
             (MCP protocol does not support interactive approval; \
             audit-only per Phase 1.5)"
        );
    }

    let ctx = ToolContext {
        conversation_id: "mcp".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
    };

    let result = tool.execute(&arguments, &ctx).await;

    // Log outcome to ring buffer. Fire-and-forget — a logging failure must
    // never affect the tool call result. Write-effectful calls get a
    // distinct `"unapproved_write"` or `"unapproved_readwrite"` tag so
    // `sovereign reflect` can surface them as a reviewable bucket
    // separate from ordinary reads.
    let base_outcome = match &result {
        Err(_) => "error",
        Ok(StepOutput::Json(v)) => {
            // Detect empty/null results to flag "index missing content" signals.
            if v.is_null() || *v == serde_json::json!({}) || *v == serde_json::json!([]) {
                "empty_result"
            } else {
                "success"
            }
        }
        Ok(_) => "success",
    };
    let outcome = match (base_outcome, descriptor.effect) {
        ("success", Effect::Write) => "unapproved_write",
        ("success", Effect::ReadWrite) => "unapproved_readwrite",
        (other, _) => other,
    };
    let _ = logger.log_tool_call(&session_id, &name, outcome).await;

    // The session call counter is kept for telemetry / rate-limit
    // decisions even though the periodic reflection nudge was removed
    // in Phase 2. Tools now surface their salient state via
    // `Tool::signal()` which the ReasonWithTools preamble polls every
    // turn — the 10-call text nudge ("Consider calling
    // session_reflection…") is obsolete.
    let _ = call_counter.fetch_add(1, Ordering::Relaxed);

    match result {
        Ok(StepOutput::Text(text)) => JsonRpcResponse::ok(id, call_tool_text(text, false)),
        Ok(StepOutput::Json(value)) => {
            let text = serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| value.to_string());
            JsonRpcResponse::ok(id, call_tool_text(text, false))
        }
        Ok(other) => JsonRpcResponse::ok(id, call_tool_text(format!("{other:?}"), false)),
        Err(e) => JsonRpcResponse::ok(
            id,
            call_tool_text(format!("Tool `{name}` failed: {e}"), true),
        ),
    }
}
