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

use corpus_engine_notes::NoteStore;
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
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        }
    }
    fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
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

/// Phase 5b broadcast surface for server-initiated MCP
/// notifications.
///
/// MCP defines `notifications/tools/list_changed` as a
/// server-pushed signal that the tool list has changed and the
/// client should refetch. We deliver these via the SSE channel
/// (`GET /mcp`) — a long-lived stream every spec-compliant client
/// opens after `initialize`.
///
/// Internally a [`tokio::sync::broadcast::Sender`] fans out one
/// payload to every connected SSE subscriber. A bounded buffer
/// (16 messages) is plenty: clients are expected to refetch on
/// any signal, so queued duplicates collapse into one re-fetch.
/// A lagging subscriber drops oldest items — we tolerate the loss
/// because the client's next `tools/list` re-syncs the truth.
///
/// Construct via [`McpNotifier::new`] and pass into
/// [`mcp_router`]. The watcher in `sovereign_tools::spec_watcher`
/// calls [`McpNotifier::notify_tools_list_changed`] from its
/// `on_change` callback when a spec event lands.
#[derive(Clone)]
pub struct McpNotifier {
    sender: std::sync::Arc<tokio::sync::broadcast::Sender<Value>>,
}

impl McpNotifier {
    /// Buffer size for the broadcast channel. Subscribers that lag
    /// past this many messages will see [`broadcast::error::RecvError::Lagged`]
    /// and we silently skip the missed entries. Tools/list_changed
    /// is idempotent (the client refetches), so dropping is fine.
    const BUFFER_SIZE: usize = 16;

    /// Build a fresh notifier with no subscribers. Subscriptions
    /// happen lazily as SSE clients connect.
    pub fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(Self::BUFFER_SIZE);
        Self {
            sender: std::sync::Arc::new(sender),
        }
    }

    /// Push a `notifications/tools/list_changed` JSON-RPC frame to
    /// every connected SSE client. No-op if there are no
    /// subscribers (e.g. during startup before any client opens
    /// `GET /mcp`).
    pub fn notify_tools_list_changed(&self) {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/tools/list_changed"
        });
        // `send` returns Err only when there are zero receivers —
        // not an error condition for us.
        let _ = self.sender.send(payload);
    }

    /// Subscribe a new SSE handler to the broadcast. Each handler
    /// gets its own independent receive cursor.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Value> {
        self.sender.subscribe()
    }
}

impl Default for McpNotifier {
    fn default() -> Self {
        Self::new()
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
    let pattern_matcher = Arc::new(sovereign_tools::notes::patterns::ToolPatternMatcher::new(
        Arc::clone(&logger),
    ));
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
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(tools))
        .layer(Extension(logger))
        .layer(Extension(Arc::new(session_id)))
        .layer(Extension(call_counter))
        .layer(Extension(feature_root))
        .layer(Extension(notifier))
        .layer(Extension(pattern_matcher))
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

/// Emit the `endpoint` event required by the 2024-11-05 HTTP+SSE transport,
/// then forward server-pushed JSON-RPC notifications from the
/// [`McpNotifier`] broadcast (Phase 5b — currently just
/// `notifications/tools/list_changed`).
///
/// Clients open this stream first, wait for the endpoint URL, then POST
/// JSON-RPC messages to it. We point them back at `/mcp` itself so both
/// transports converge on the same handler.
///
/// Each broadcast payload ships as an unnamed SSE `data:` event whose
/// body is the JSON-RPC frame — the format MCP clients already parse
/// for server-sent notifications. A lagging subscriber that misses
/// items (the broadcast buffer is small) silently drops them; the
/// notification is idempotent (the client refetches on any signal),
/// so the next event re-syncs.
async fn mcp_sse(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(notifier): Extension<McpNotifier>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    if !is_localhost(&peer) {
        return Err(StatusCode::FORBIDDEN);
    }
    let endpoint_event = stream::once(async {
        Ok::<_, Infallible>(Event::default().event("endpoint").data("/mcp"))
    });
    // Subscribe per-connection so each client gets its own cursor.
    // BroadcastStream maps lagged-receiver errors to a stream Err,
    // which we surface as a JSON `{"error":"lagged"}` event so the
    // client can refetch defensively. Empty stream items between
    // notifications are kept alive by axum's KeepAlive ping.
    let rx = notifier.subscribe();
    let notifications = tokio_stream::wrappers::BroadcastStream::new(rx).map(|res| {
        match res {
            Ok(payload) => {
                let body = payload.to_string();
                Ok::<_, Infallible>(Event::default().data(body))
            }
            Err(_lagged) => {
                // Don't log every lag — for tools/list the client's
                // next refetch is the truth anyway. Emit a blank
                // data event so well-behaved clients refetch
                // defensively.
                Ok::<_, Infallible>(
                    Event::default()
                        .data(r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#),
                )
            }
        }
    });
    Ok(Sse::new(endpoint_event.chain(notifications)).keep_alive(KeepAlive::default()))
}

/// Single JSON-RPC handler for both `/mcp` and `/mcp/message`.
/// Notifications (requests without an `id`) receive an empty 202 response
/// — per JSON-RPC 2.0, notifications have no reply. Accepts both a single
/// request object and a batch array (batches are a MUST in the 2025-03-26
/// MCP revision; removed again in 2025-06-18, so we take either).
async fn mcp_handle(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(tools): Extension<Arc<ToolRegistry>>,
    Extension(logger): Extension<Arc<NoteStore>>,
    Extension(session_id): Extension<Arc<String>>,
    Extension(call_counter): Extension<Arc<AtomicU64>>,
    Extension(feature_root): Extension<FeatureRoot>,
    Extension(pattern_matcher): Extension<
        Arc<sovereign_tools::notes::patterns::ToolPatternMatcher>,
    >,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    if !is_localhost(&peer) {
        return (
            StatusCode::FORBIDDEN,
            Json(JsonRpcResponse::err(
                Value::Null,
                -32001,
                "MCP is local-only",
            )),
        )
            .into_response();
    }

    // Agent identity for the work atlas. Prefer the explicit header
    // the agent supplies (`X-Agent-Session`); fall back to the
    // per-MCP-connection `session_id` so we still get session
    // grouping for clients that don't set the header. Used only by
    // tools that read `ToolContext::agent_session_token`; everything
    // else ignores it.
    let agent_session_token = headers
        .get("x-agent-session")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("conn:{}", session_id.as_str()));

    match body {
        Value::Array(items) => {
            // JSON-RPC batch. An empty array is invalid per the spec.
            if items.is_empty() {
                return (
                    StatusCode::OK,
                    Json(JsonRpcResponse::err(Value::Null, -32600, "empty batch")),
                )
                    .into_response();
            }
            let mut responses = Vec::new();
            for item in items {
                match serde_json::from_value::<JsonRpcRequest>(item) {
                    Ok(req) => {
                        if let Some(response) = dispatch(
                            req,
                            Arc::clone(&tools),
                            Arc::clone(&logger),
                            Arc::clone(&session_id),
                            Arc::clone(&call_counter),
                            feature_root.clone(),
                            Arc::clone(&pattern_matcher),
                            agent_session_token.clone(),
                        )
                        .await
                        {
                            responses.push(response);
                        }
                    }
                    Err(_) => {
                        responses.push(JsonRpcResponse::err(Value::Null, -32600, "invalid request"))
                    }
                }
            }
            if responses.is_empty() {
                // All notifications — nothing to reply with.
                StatusCode::ACCEPTED.into_response()
            } else {
                (StatusCode::OK, Json(responses)).into_response()
            }
        }
        single => match serde_json::from_value::<JsonRpcRequest>(single) {
            Ok(req) => match dispatch(
                req,
                tools,
                logger,
                session_id,
                call_counter,
                feature_root,
                pattern_matcher,
                agent_session_token,
            )
            .await
            {
                Some(response) => (StatusCode::OK, Json(response)).into_response(),
                None => StatusCode::ACCEPTED.into_response(),
            },
            Err(_) => (
                StatusCode::OK,
                Json(JsonRpcResponse::err(Value::Null, -32600, "invalid request")),
            )
                .into_response(),
        },
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
    pattern_matcher: Arc<sovereign_tools::notes::patterns::ToolPatternMatcher>,
    agent_session_token: String,
) -> Option<JsonRpcResponse> {
    // Notifications: no id → no response. We still want to accept the
    // method (e.g. `notifications/initialized`) so the client doesn't see
    // an error. Return None so the handler sends 202 Accepted.
    let Some(id) = req.id else {
        tracing::debug!(method = %req.method, "mcp: notification received");
        return None;
    };

    let response = match req.method.as_str() {
        "initialize" => {
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
        "tools/call" => {
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
    pattern_matcher: Arc<sovereign_tools::notes::patterns::ToolPatternMatcher>,
    agent_session_token: String,
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

    // Glassbox the dispatch so operators can correlate work-atlas
    // session creation with the MCP call that triggered it (ARCH §9.1).
    // Truncate the token to 12 chars per ARCH §9.3 — redact deliberately.
    let token_redacted: String = agent_session_token.chars().take(12).collect();
    tracing::debug!(
        tool = %name,
        agent_session_token = %token_redacted,
        "mcp:tool_call dispatched"
    );

    let ctx = ToolContext {
        conversation_id: "mcp".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: Some(agent_session_token),
        turn_index: 0,
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

    match result {
        Ok(StepOutput::Text(text)) => JsonRpcResponse::ok(id, call_tool_text(text, false)),
        Ok(StepOutput::Json(value)) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            JsonRpcResponse::ok(id, call_tool_text(text, false))
        }
        Ok(other) => JsonRpcResponse::ok(id, call_tool_text(format!("{other:?}"), false)),
        Err(e) => JsonRpcResponse::ok(
            id,
            call_tool_text(format!("Tool `{name}` failed: {e}"), true),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `McpNotifier::notify_tools_list_changed` delivers the
    /// JSON-RPC frame to every subscriber. Two subscribers each
    /// see the same payload — independent cursors, broadcast fanout.
    #[tokio::test]
    async fn notifier_fans_out_tools_list_changed_to_subscribers() {
        let n = McpNotifier::new();
        let mut a = n.subscribe();
        let mut b = n.subscribe();

        n.notify_tools_list_changed();

        let recv_a = tokio::time::timeout(std::time::Duration::from_secs(1), a.recv())
            .await
            .expect("subscriber A should receive within 1s")
            .expect("payload arrives");
        let recv_b = tokio::time::timeout(std::time::Duration::from_secs(1), b.recv())
            .await
            .expect("subscriber B should receive within 1s")
            .expect("payload arrives");

        assert_eq!(
            recv_a, recv_b,
            "both subscribers must see the same broadcast payload"
        );
        assert_eq!(
            recv_a["method"], "notifications/tools/list_changed",
            "method name must match MCP spec"
        );
        assert_eq!(recv_a["jsonrpc"], "2.0");
    }

    /// Publishing with no subscribers is a no-op (broadcast::send
    /// returns Err, which we swallow). The notifier must not panic
    /// or block in this case — common during startup.
    #[test]
    fn notifier_publish_with_no_subscribers_is_noop() {
        let n = McpNotifier::new();
        // Just check it doesn't panic. The `let _ = ...` inside
        // `notify_tools_list_changed` swallows the no-receivers err.
        n.notify_tools_list_changed();
    }
}
