//! MCP HTTP server endpoints.
//!
//! Exposes Sovereign's Code Intelligence tools to any MCP-compatible
//! coding agent (Claude Code, Cursor, Cline, OmO, …) at:
//!
//! - `POST /mcp`          initialize handshake (JSON-RPC 2.0)
//! - `POST /mcp/message`  tool discovery + invocation
//! - `GET  /mcp`          SSE keep-alive stream (MCP requires one; v1
//!                        emits no server-initiated notifications)
//!
//! **Not to be confused with `sovereign-tools/src/mcp/`**, which is the
//! *client* side — consuming external MCP servers. This module is the
//! *server* side, hosting our own tools over the MCP wire format.
//!
//! ## Security — localhost only
//!
//! The MCP server is intended for local dev-agent integration only.
//! Every handler checks the peer address via `ConnectInfo<SocketAddr>`
//! and rejects anything that isn't `127.0.0.1` / `::1`. The auth
//! middleware that gates `/v1/*` is intentionally skipped on these
//! routes so local agents don't need API keys.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use sovereign_core::registry::ToolRegistry;
use sovereign_core::runtime::Runtime;
use sovereign_core::types::{StepOutput, ToolContext};

// ─── JSON-RPC 2.0 envelope types ──────────────────────────────

/// Inbound JSON-RPC request. `params` is opaque JSON — each method is
/// responsible for parsing its own shape. We deserialize `jsonrpc` so
/// the envelope round-trips cleanly but don't validate its value (the
/// spec says "2.0"; we accept anything for forward-compat with 3.0
/// discussions and clients that omit it).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    #[serde(default)]
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// Outbound JSON-RPC response. Either `result` or `error` is set.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error object. Spec codes: -32700 parse, -32600 invalid
/// request, -32601 method not found, -32602 invalid params, -32603
/// internal error. Tool-call failures that the agent should *see* (not
/// break on) use `CallToolResult { isError: true }` inside a successful
/// `result` envelope instead — MCP's distinction between "transport
/// failed" and "tool said no".
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    fn result(id: Value, value: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// ─── Router ───────────────────────────────────────────────────

/// Build the MCP route fragment. Merged into the top-level server
/// router in `main.rs` *outside* the auth middleware layer — these
/// routes don't want API keys, they want localhost addresses.
pub fn mcp_router() -> Router {
    Router::new()
        .route("/mcp", post(mcp_initialize).get(mcp_sse))
        .route("/mcp/message", post(mcp_message))
        .route("/mcp/stats", axum::routing::get(mcp_stats))
}

// ─── Localhost gate ───────────────────────────────────────────

fn is_localhost(addr: &SocketAddr) -> bool {
    let ip = addr.ip();
    ip.is_loopback()
}

// ─── Handlers ─────────────────────────────────────────────────

async fn mcp_initialize(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if !is_localhost(&peer) {
        return (
            StatusCode::FORBIDDEN,
            Json(JsonRpcResponse::error(
                req.id,
                -32001,
                "MCP is local-only — remote access is refused",
            )),
        )
            .into_response();
    }

    let session_id = generate_session_id(&req.params);
    let result = serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "sovereign",
            "version": env!("CARGO_PKG_VERSION")
        },
        "sessionId": session_id
    });

    (StatusCode::OK, Json(JsonRpcResponse::result(req.id, result))).into_response()
}

/// Generate a session ID that encodes the username for cross-session note attribution.
/// Format: `{username}-{YYYY-MM-DDTHH:MM}-{uuid6}` where username comes from
/// `params.clientInfo.name` or `params.meta.userName` (OmO convention).
fn generate_session_id(params: &Option<Value>) -> String {
    let username = params
        .as_ref()
        .and_then(|p| {
            p.get("clientInfo")
                .and_then(|c| c.get("name"))
                .and_then(|v| v.as_str())
                .or_else(|| p.get("meta").and_then(|m| m.get("userName")).and_then(|v| v.as_str()))
        })
        .unwrap_or("user");
    let slug: String = username
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let slug = if slug.is_empty() { "user".to_string() } else { slug };
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M");
    let uid = &uuid::Uuid::new_v4().to_string()[..6];
    format!("{slug}-{ts}-{uid}")
}

async fn mcp_sse(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, StatusCode>
{
    if !is_localhost(&peer) {
        return Err(StatusCode::FORBIDDEN);
    }
    // v1 has no server-initiated notifications — return a stream that
    // never produces events but does ping the client via keep-alive so
    // idle connections don't get dropped by intermediaries.
    let s = stream::pending::<Result<Event, std::convert::Infallible>>();
    Ok(Sse::new(s).keep_alive(KeepAlive::default()))
}

async fn mcp_message(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(runtime): Extension<Arc<Runtime>>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if !is_localhost(&peer) {
        return (
            StatusCode::FORBIDDEN,
            Json(JsonRpcResponse::error(
                req.id,
                -32001,
                "MCP is local-only — remote access is refused",
            )),
        )
            .into_response();
    }

    let id = req.id.clone();
    let response = match req.method.as_str() {
        "tools/list" => handle_tools_list(&runtime.tools, id),
        "tools/call" => handle_tools_call(&runtime.tools, req.params, id).await,
        "initialize" => {
            // Some clients send initialize as a regular message rather
            // than via the POST /mcp handshake. Accept both.
            let session_id = generate_session_id(&req.params);
            let result = serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "sovereign",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "sessionId": session_id
            });
            JsonRpcResponse::result(id, result)
        }
        other => JsonRpcResponse::error(id, -32601, format!("method not found: {other}")),
    };

    (StatusCode::OK, Json(response)).into_response()
}

// ─── GET /mcp/stats ───────────────────────────────────────────

async fn mcp_stats(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(runtime): Extension<Arc<Runtime>>,
) -> impl IntoResponse {
    if !is_localhost(&peer) {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "local-only"}))).into_response();
    }

    let counts = runtime.tools.call_counts();
    let total: u64 = counts.iter().map(|(_, n)| n).sum();

    let tools_json: Vec<serde_json::Value> = counts
        .into_iter()
        .map(|(name, count)| serde_json::json!({ "tool": name, "calls": count }))
        .collect();

    (StatusCode::OK, Json(serde_json::json!({
        "total_calls": total,
        "tools": tools_json,
    }))).into_response()
}

// ─── tools/list ───────────────────────────────────────────────

// MCP allowlist + alias logic lives in
// [`sovereign_tools::mcp_surface`]. The standalone server and the
// embedded daemon's `sovereign-mesh::mcp_router` share that module
// so they advertise the same surface to clients.
use sovereign_tools::mcp_surface::{is_mcp_exposed, render_tools_list, resolve_alias};

pub(crate) fn handle_tools_list(registry: &ToolRegistry, id: Value) -> JsonRpcResponse {
    let descriptors = registry.descriptors();
    let tools = render_tools_list(&descriptors);
    JsonRpcResponse::result(id, serde_json::json!({ "tools": tools }))
}

// ─── tools/call ───────────────────────────────────────────────

/// Explicitly unsupported tool names that we refuse honestly. Cross-file
/// references and impact analysis are not yet implemented. Any agent
/// asking for these gets a useful message back, not an error or a
/// hallucinated answer.
const UNSUPPORTED_TOOLS: &[&str] = &["find_references", "impact_analysis"];

pub(crate) async fn handle_tools_call(
    registry: &ToolRegistry,
    params: Option<Value>,
    id: Value,
) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => return JsonRpcResponse::error(id, -32602, "missing params"),
    };

    let raw_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return JsonRpcResponse::error(id, -32602, "missing 'name' in params"),
    };
    // Alias rewrite: a client that cached a legacy name (e.g.
    // `find_callers`) hits the same canonical handler (`callers`)
    // as a fresh client. Telemetry is recorded against the
    // canonical name so call counts aggregate across both
    // spellings.
    let name = resolve_alias(&raw_name).to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    // Honest refusal for unsupported tools. Returns a successful
    // result envelope with `isError: false` and a helpful message so
    // the agent's loop can keep going.
    if UNSUPPORTED_TOOLS.contains(&raw_name.as_str()) {
        return JsonRpcResponse::result(
            id,
            call_tool_text(
                format!(
                    "`{raw_name}` is not available in this version. \
                     I can find the symbol definition with `symbols` \
                     and semantically similar code with `code_search` — \
                     would either help?"
                ),
                false,
            ),
        );
    }

    // Only route MCP-exposed tools. An agent asking for a
    // tool ID we host internally but don't expose (e.g. `shell`) gets
    // a method-not-found error — keeps the MCP surface bounded to the
    // coding-agent use case.
    if !is_mcp_exposed(&name) {
        return JsonRpcResponse::error(id, -32601, format!("tool not found: {raw_name}"));
    }

    let tool = match registry.get(&name) {
        Ok(t) => t,
        Err(_) => {
            return JsonRpcResponse::result(
                id,
                call_tool_text(
                    format!(
                        "`{name}` is not registered in this server build. \
                         Rebuild with `corpus-engine/treesitter` enabled and \
                         index a code corpus via `sovereign code index`."
                    ),
                    false,
                ),
            );
        }
    };

    // Validate before execute so bad input produces a user-readable
    // error rather than a panic inside a tool.
    if let Err(e) = tool.validate(&arguments) {
        return JsonRpcResponse::result(id, call_tool_text(e.to_string(), true));
    }

    registry.record_call(&name);

    let ctx = ToolContext {
        conversation_id: "mcp".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
    };

    match tool.execute(&arguments, &ctx).await {
        Ok(StepOutput::Text(text)) => JsonRpcResponse::result(id, call_tool_text(text, false)),
        Ok(StepOutput::Json(value)) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            JsonRpcResponse::result(id, call_tool_text(text, false))
        }
        Ok(other) => JsonRpcResponse::result(
            id,
            call_tool_text(format!("{other:?}"), false),
        ),
        Err(e) => JsonRpcResponse::result(
            id,
            call_tool_text(
                format!("Tool `{name}` failed: {e}"),
                true,
            ),
        ),
    }
}

/// Build a `CallToolResult` shape per the MCP spec:
/// `{ content: [{ type: "text", text }], isError }`.
fn call_tool_text(text: impl Into<String>, is_error: bool) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": text.into(),
        }],
        "isError": is_error,
    })
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::registry::ToolRegistry;

    fn empty_engine() -> Arc<corpus_engine::CorpusEngine> {
        let tmp = std::env::temp_dir().join(format!(
            "sov-mcp-test-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let embed: corpus_engine::EmbedFn = Arc::new(|_text: &str| {
            Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
        });
        Arc::new(corpus_engine::CorpusEngine::new(
            tmp.clone(),
            tmp,
            embed,
        ))
    }

    fn registry_with_code_tools() -> ToolRegistry {
        let engine = empty_engine();
        let graph: sovereign_tools::ScipGraphHandle = Arc::new(arc_swap::ArcSwap::from_pointee(
            corpus_engine::ScipGraph::open_in_memory("test")
                .expect("in-memory ScipGraph"),
        ));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(sovereign_tools::SymbolLookupTool::new(
            Arc::clone(&engine),
        )));
        registry.register(Box::new(sovereign_tools::CodeSearchTool::new(
            Arc::clone(&engine),
        )));
        registry.register(Box::new(sovereign_tools::RecentChangesTool::new(
            Arc::clone(&engine),
        )));
        registry.register(Box::new(sovereign_tools::FindCalleesTool::new(
            Arc::clone(&engine),
            Arc::clone(&graph),
        )));
        registry.register(Box::new(sovereign_tools::FindCallersTool::new(
            Arc::clone(&engine),
            Arc::clone(&graph),
        )));
        registry
    }

    // ─── JSON-RPC envelope tests ─────────────────────────────

    #[test]
    fn json_rpc_response_elides_absent_field() {
        let ok = JsonRpcResponse::result(Value::from(1), Value::Null);
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));

        let err = JsonRpcResponse::error(Value::from(2), -32601, "method not found");
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
    }

    #[test]
    fn is_localhost_accepts_ipv4_and_ipv6_loopback() {
        let v4: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let v6: SocketAddr = "[::1]:9999".parse().unwrap();
        let lan: SocketAddr = "192.168.1.5:9999".parse().unwrap();
        assert!(is_localhost(&v4));
        assert!(is_localhost(&v6));
        assert!(!is_localhost(&lan));
    }

    #[test]
    fn call_tool_text_shape() {
        let v = call_tool_text("hi", false);
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "hi");
        assert_eq!(v["isError"], false);
    }

    #[test]
    fn unsupported_tools_covered() {
        for t in ["find_references", "impact_analysis"] {
            assert!(UNSUPPORTED_TOOLS.contains(&t));
        }
    }

    /// The drift-guard moved to `sovereign_tools::mcp_surface` —
    /// the `MCP_TOOLS_ALWAYS` allowlist + `MCP_TOOL_ALIASES` map
    /// are unit-tested in that module so both the standalone
    /// server and the embedded daemon's mcp_router test the same
    /// contract once. Smoke-check the import resolves here so a
    /// mis-merged refactor surfaces immediately.
    #[test]
    fn mcp_surface_exports_the_renamed_canonicals() {
        use sovereign_tools::mcp_surface::{is_mcp_exposed, resolve_alias};
        // Renamed canonical ids are exposed.
        for new in &["symbols", "callers", "callees", "blast", "note", "notes"] {
            assert!(is_mcp_exposed(new), "{new} should be MCP-exposed");
        }
        // Legacy ids alias-rewrite to the renamed canonical.
        assert_eq!(resolve_alias("find_callers"), "callers");
        assert_eq!(resolve_alias("write_note"), "note");
    }

    // ─── T-15: tools/list emits canonical + deprecated aliases ───
    //
    // The test registry registers the 3 SCIP code-intel tools that
    // have been renamed in this build (`symbols`, `callers`,
    // `callees`). `tools/list` should advertise those 3 canonical
    // names plus 3 deprecated mirror entries (`symbol_lookup`,
    // `find_callers`, `find_callees`) — 6 total — so cached agent
    // clients keep working until they refresh. Out-of-scope tool
    // ids (`find_references`, retired ATOS lifecycle tools, etc.)
    // never appear.
    //
    // The full production server registers more tools (notes,
    // lint, blast); this minimal harness keeps the test fast and
    // independent of `corpus-engine/treesitter`.
    #[tokio::test]
    async fn t15_tools_list_emits_canonical_plus_deprecated_aliases() {
        let registry = registry_with_code_tools();
        let resp = handle_tools_list(&registry, Value::from(1));

        let result = resp.result.expect("tools/list returns a result");
        let tools = result["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();

        // Canonical (renamed) ids appear.
        for required in &["symbols", "callers", "callees"] {
            assert!(
                names.contains(required),
                "Required tool `{required}` missing from tools/list — got {names:?}"
            );
        }

        // Deprecated aliases also appear, marked as such, so a
        // cached client keeps working.
        for legacy in &["symbol_lookup", "find_callers", "find_callees"] {
            let entry = tools
                .iter()
                .find(|t| t["name"].as_str() == Some(legacy))
                .unwrap_or_else(|| panic!("alias `{legacy}` missing from tools/list"));
            let desc = entry["description"].as_str().unwrap_or("");
            assert!(
                desc.starts_with("(deprecated alias for `"),
                "alias `{legacy}` description should be marked deprecated, got {desc:?}"
            );
        }

        // Out-of-scope / unsupported tool ids never appear, even if
        // they were registered (they aren't in this minimal harness).
        for excluded in &[
            "find_references",
            "impact_analysis",
            "provision_feature",
            "session_reflection",
        ] {
            assert!(
                !names.contains(excluded),
                "Out-of-scope tool `{excluded}` appeared in tools/list — got {names:?}"
            );
        }

        // 3 canonicals + 3 deprecated mirrors = 6.
        assert_eq!(
            tools.len(),
            6,
            "expected 3 canonical + 3 alias mirrors, got {names:?}"
        );
    }

    // ─── T-16: Honest refusal for unsupported tools ──────────

    #[tokio::test]
    async fn t16_unsupported_tools_return_honest_refusal() {
        let registry = registry_with_code_tools();

        for tool in &["find_references", "impact_analysis"] {
            let params = serde_json::json!({
                "name": tool,
                "arguments": { "symbol": "execute_step" }
            });
            let resp = handle_tools_call(&registry, Some(params), Value::from(1)).await;

            // Must be a successful result envelope (not JSON-RPC error)
            assert!(
                resp.error.is_none(),
                "Tool `{tool}` returned JSON-RPC error instead of graceful result"
            );
            let result = resp.result.expect("unsupported tool returns result");

            // isError must be false — agent must keep looping
            assert_eq!(
                result["isError"].as_bool(),
                Some(false),
                "isError must be false for unsupported tool `{tool}`"
            );

            let text = result["content"][0]["text"].as_str().unwrap_or("");
            assert!(
                text.contains("symbol_lookup") || text.contains("code_search"),
                "Refusal for `{tool}` must suggest alternatives; got: {text}"
            );
        }
    }

    // ─── T-17: Injection payloads rejected safely ────────────

    #[tokio::test]
    async fn t17_filter_injection_rejected() {
        let registry = registry_with_code_tools();

        let payloads = [
            "foo'; DROP TABLE chunks; --",
            "foo\" OR 1=1 --",
            "'; SELECT * FROM chunks WHERE '1'='1",
        ];

        for name in &payloads {
            let params = serde_json::json!({
                "name": "symbol_lookup",
                "arguments": { "name": name }
            });
            let resp = handle_tools_call(&registry, Some(params), Value::from(1)).await;

            // Must be a successful envelope, not a JSON-RPC error.
            assert!(
                resp.error.is_none(),
                "injection payload `{name}` produced JSON-RPC error"
            );
            let result = resp.result.expect("injection produces result envelope");

            // isError must be true — validation rejected it.
            assert_eq!(
                result["isError"].as_bool(),
                Some(true),
                "isError must be true for injection payload `{name}`"
            );

            let text = result["content"][0]["text"].as_str().unwrap_or("");

            // Response must not leak backend internals — no mention of
            // LanceDB, SQLite, or "syntax error".
            let lower = text.to_lowercase();
            assert!(
                !lower.contains("lancedb")
                    && !lower.contains("sqlite")
                    && !lower.contains("syntax error"),
                "Injection payload `{name}` leaked internals: {text}"
            );

            // Must actually mention the invalid input somehow so the
            // agent can course-correct.
            assert!(
                text.contains("invalid") || text.contains("symbol name"),
                "Rejection message for `{name}` not user-actionable: {text}"
            );
        }
    }

    // ─── Extra: graceful empty-result path for symbol_lookup ─

    #[tokio::test]
    async fn symbol_lookup_no_results_is_not_an_error() {
        let registry = registry_with_code_tools();
        let params = serde_json::json!({
            "name": "symbol_lookup",
            "arguments": { "name": "nonexistent_xyz_abc" }
        });
        let resp = handle_tools_call(&registry, Some(params), Value::from(1)).await;

        assert!(resp.error.is_none());
        let result = resp.result.expect("empty result returns envelope");
        assert_eq!(result["isError"].as_bool(), Some(false));
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.to_lowercase().contains("no symbol") || text.contains("not found"),
            "empty result should announce not-found: {text}"
        );
        assert!(
            text.contains("code_search"),
            "empty result should suggest code_search fallback: {text}"
        );
    }

    // ─── Session ID generation ────────────────────────────────

    #[test]
    fn session_id_extracts_client_info_name() {
        let params = Some(serde_json::json!({
            "clientInfo": { "name": "alice" }
        }));
        let id = generate_session_id(&params);
        assert!(id.starts_with("alice-"), "session id must start with the slugified username");
    }

    #[test]
    fn session_id_falls_back_to_meta_username() {
        let params = Some(serde_json::json!({
            "meta": { "userName": "BobSmith" }
        }));
        let id = generate_session_id(&params);
        assert!(id.starts_with("bobsmith-"), "meta.userName must be used when clientInfo absent");
    }

    #[test]
    fn session_id_falls_back_to_user_when_no_params() {
        let id = generate_session_id(&None);
        assert!(id.starts_with("user-"), "no params must produce 'user-...' session id");
    }

    #[test]
    fn session_id_slugifies_special_characters() {
        let params = Some(serde_json::json!({
            "clientInfo": { "name": "Alice@Corp.io" }
        }));
        let id = generate_session_id(&params);
        // '@' and '.' become '-' then trimmed; "alice-corp-io" is the slug
        assert!(
            id.starts_with("alice-corp-io-") || id.starts_with("alice"),
            "special chars must be slugified: got {id}"
        );
        assert!(!id.contains('@'), "@ must not appear in session id");
        assert!(!id.contains('.'), ". must not appear in session id");
    }

    #[test]
    fn session_id_format_has_three_dash_separated_segments() {
        // Format: {slug}-{YYYY-MM-DDTHH:MM}-{uuid6}
        // The timestamp contains a '-' and 'T', and uuid6 is 6 hex chars.
        // Total: slug | date-part | uuid6 — joined by '-' with timestamp containing '-'.
        let id = generate_session_id(&None);
        let parts: Vec<&str> = id.splitn(3, '-').collect();
        // "user" | "YYYY" | rest
        assert_eq!(parts[0], "user");
        // Second part should be a year (4 digits starting with 20xx)
        assert!(
            parts[1].parse::<u32>().is_ok() && parts[1].starts_with("20"),
            "second segment must be a year: {}", parts[1]
        );
    }

    // ─── Extra: tool_not_found for non-MCP-exposed tool ──────

    #[tokio::test]
    async fn tool_not_found_is_jsonrpc_error() {
        let registry = registry_with_code_tools();
        let params = serde_json::json!({
            "name": "shell",
            "arguments": {}
        });
        let resp = handle_tools_call(&registry, Some(params), Value::from(1)).await;
        // shell isn't in MCP_EXPOSED_TOOLS — should be a JSON-RPC error
        // (-32601 method not found), not a successful CallToolResult.
        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
    }
}
