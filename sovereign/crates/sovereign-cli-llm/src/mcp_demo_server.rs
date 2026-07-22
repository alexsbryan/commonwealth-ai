// SPDX-License-Identifier: AGPL-3.0-or-later
//! A tiny reference MCP server for the end-to-end demo (`svrn mcp
//! demo-server`).
//!
//! It exposes one tool, `get_clearance_code(agent)`, returning a **sealed**
//! code that exists nowhere else — not in any model's weights, not in any
//! corpus. So when `svrn chat "what is Vega's clearance code?"` answers
//! `TR-7741-Q`, that answer is *proof* the model actually selected and called
//! the MCP tool rather than fabricating — the whole point of grounding the
//! feature in a demonstrable use case.
//!
//! The protocol is the minimal JSON-RPC-over-POST that
//! `sovereign_tools::mcp::http::HttpSseTransport` speaks: `initialize`,
//! `tools/list`, `tools/call`, and an ack for the `notifications/initialized`
//! notification. Modeled on `sovereign-server/src/routes_mcp.rs`; kept
//! self-contained so the demo needs no daemon, corpus, or model.

use std::net::SocketAddr;

use axum::{routing::post, Json, Router};
use serde_json::{json, Value};

/// Sealed clearance codes. These strings exist ONLY here — fabricating them is
/// not possible, so a correct answer proves a real tool call.
const CLEARANCE: &[(&str, &str)] = &[
    ("vega", "TR-7741-Q"),
    ("orion", "BX-2208-K"),
    ("lyra", "ZD-5193-M"),
];

const TOOL_NAME: &str = "get_clearance_code";
const TOOL_DESCRIPTION: &str = "Look up a field agent's secure clearance code \
from the classified roster by code name. This is the ONLY source of an agent's \
clearance code — call it whenever a clearance code is requested.";

const MEMO_TOOL_NAME: &str = "read_memo";
const MEMO_TOOL_DESCRIPTION: &str = "Read the text contents of a local file (a \
memo, note, transcript, or any text file) at a given filesystem path. Use this \
to read a file the user attached or referenced by its path. Demonstrates the \
attach-a-file-for-tools shape: a tool that takes a path and reads that file.";

fn clearance_for(agent: &str) -> Option<&'static str> {
    let needle = agent.trim().to_lowercase();
    CLEARANCE
        .iter()
        .find(|(name, _)| *name == needle)
        .map(|(_, code)| *code)
}

/// The MCP `tools/list` entry for the demo tool. A distinctive description (so
/// it clears the router's tool-relevance gate) and a one-arg required schema
/// (from which the adapter synthesizes an example call).
fn tool_entry() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": TOOL_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "The agent's code name, e.g. Vega."
                }
            },
            "required": ["agent"]
        }
    })
}

/// The `tools/list` entry for the file-reading demo tool — the vision/audio
/// shape (a tool that takes a `path` and reads that file).
fn memo_tool_entry() -> Value {
    json!({
        "name": MEMO_TOOL_NAME,
        "description": MEMO_TOOL_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to read."
                }
            },
            "required": ["path"]
        }
    })
}

fn rpc_result(id: Value, result: Value) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn rpc_error(id: Value, code: i64, message: String) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
}

async fn handle(Json(req): Json<Value>) -> Json<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

    match method {
        "initialize" => rpc_result(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "sovereign-mcp-demo", "version": "0.1.0" }
            }),
        ),
        "tools/list" => rpc_result(id, json!({ "tools": [ tool_entry(), memo_tool_entry() ] })),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let (text, is_error) = match name {
                TOOL_NAME => {
                    let agent = args.get("agent").and_then(|v| v.as_str()).unwrap_or("");
                    match clearance_for(agent) {
                        Some(code) => (format!("Agent {agent}'s clearance code is {code}."), false),
                        None => (
                            format!("No agent named '{agent}' is on the classified roster."),
                            true,
                        ),
                    }
                }
                MEMO_TOOL_NAME => {
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    match std::fs::read_to_string(path) {
                        Ok(contents) => (contents, false),
                        Err(e) => (format!("Could not read '{path}': {e}"), true),
                    }
                }
                other => return rpc_error(id, -32601, format!("unknown tool: {other}")),
            };
            rpc_result(
                id,
                json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error }),
            )
        }
        // Notifications (e.g. `notifications/initialized`) carry no id and want
        // no result — just a 200 ack.
        _ if id.is_null() => Json(json!({})),
        _ => rpc_error(id, -32601, format!("method not found: {method}")),
    }
}

/// The reference server's router. Shared by the `demo-server` subcommand and
/// the e2e test so both exercise the same code.
pub fn reference_mcp_router() -> Router {
    Router::new().route("/mcp", post(handle))
}

/// `svrn mcp demo-server [--port N]` — run the reference server until the
/// process exits.
pub async fn run_demo_server(args: &[String]) -> i32 {
    let mut port: u16 = 4319;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--port" {
            i += 1;
            match args.get(i).and_then(|s| s.parse::<u16>().ok()) {
                Some(p) => port = p,
                None => {
                    eprintln!("--port needs a number");
                    return 1;
                }
            }
        }
        i += 1;
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind {addr}: {e}");
            return 1;
        }
    };
    let url = format!("http://{addr}/mcp");
    eprintln!("Reference MCP server listening — {url}");
    eprintln!();
    eprintln!("  1. svrn mcp add demo --url {url}");
    eprintln!("  2. svrn chat \"what is Vega's clearance code?\"   →  TR-7741-Q");
    eprintln!();
    eprintln!("Sealed agents: Vega, Orion, Lyra.  Ctrl-C to stop.");

    if let Err(e) = axum::serve(listener, reference_mcp_router().into_make_service()).await {
        eprintln!("server error: {e}");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::registry::ToolRegistry;
    use sovereign_core::types::StepOutput;
    use sovereign_tools::mcp::config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
    use sovereign_tools::mcp::McpServerManager;

    /// Bind the reference server on an ephemeral port and return its `/mcp` URL.
    async fn spawn() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, reference_mcp_router().into_make_service()).await;
        });
        format!("http://{addr}/mcp")
    }

    /// The whole point of the feature, proven without a model: an external MCP
    /// server's tool is discovered, registered, descriptor-enriched, and
    /// callable over real HTTP, and a sealed value round-trips into the result.
    #[tokio::test]
    async fn sealed_token_round_trips_through_the_registry() {
        let url = spawn().await;

        // 1. The config → from_config → registry path every surface uses.
        let cfg = McpServerConfig {
            name: "demo".into(),
            description: None,
            enabled: true,
            transport: McpTransportConfig::Http {
                url: url.clone(),
                auth: McpAuthConfig::None,
            },
            global: true,
        };
        let mut registry = ToolRegistry::new();
        let mgr = McpServerManager::from_config(std::slice::from_ref(&cfg), &mut registry).await;
        let statuses = mgr.server_statuses().await;
        assert!(
            statuses[0].connected,
            "reference server should connect: {:?}",
            statuses[0].error
        );

        // 2. Registered under the mcp_<server>_<tool> id, with the synthesized
        //    example call (the 'knowing how' enrichment that makes the planner
        //    emit a tool step).
        let tool = registry
            .get("mcp_demo_get_clearance_code")
            .expect("MCP tool registered");
        let desc = tool.descriptor();
        assert!(
            !desc.examples.is_empty(),
            "adapter should synthesize an example from the input schema"
        );
        assert_eq!(desc.examples[0].call, json!({ "agent": "example" }));

        // 3. Calling it returns the sealed token — proof the round-trip works.
        let ctx = sovereign_core::types::ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        };
        let out = tool
            .execute(&json!({ "agent": "Vega" }), &ctx)
            .await
            .expect("tool call succeeds");
        let text = match out {
            StepOutput::Text(t) => t,
            other => panic!("expected text output, got {other:?}"),
        };
        assert!(
            text.contains("TR-7741-Q"),
            "sealed clearance code must round-trip into the result: {text}"
        );
    }

    /// The attach-a-file-for-tools shape, proven without a model: the model
    /// passes a PATH, the tool reads THAT file. Write a fixture with a sealed
    /// marker, then prove `read_memo(path)` returns its contents through the
    /// real registry + adapter — so an attached file's path reaching a tool is
    /// demonstrable end to end.
    #[tokio::test]
    async fn attached_file_path_round_trips_through_read_memo() {
        let dir = tempfile::tempdir().unwrap();
        let memo = dir.path().join("memo.txt");
        let sealed = "SEALED-MEMO-9F3K: ship the attach-for-tools spec.";
        std::fs::write(&memo, sealed).unwrap();

        let url = spawn().await;
        let cfg = McpServerConfig {
            name: "demo".into(),
            description: None,
            enabled: true,
            transport: McpTransportConfig::Http {
                url,
                auth: McpAuthConfig::None,
            },
            global: true,
        };
        let mut registry = ToolRegistry::new();
        let _mgr = McpServerManager::from_config(std::slice::from_ref(&cfg), &mut registry).await;

        let tool = registry
            .get("mcp_demo_read_memo")
            .expect("read_memo tool registered");
        // Descriptor enrichment: a synthesized {path: …} example so the planner
        // fills the path argument.
        assert_eq!(
            tool.descriptor().examples[0].call,
            json!({ "path": "example" })
        );

        let ctx = sovereign_core::types::ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        };
        let out = tool
            .execute(&json!({ "path": memo.to_string_lossy() }), &ctx)
            .await
            .expect("read_memo call succeeds");
        let text = match out {
            StepOutput::Text(t) => t,
            other => panic!("expected text output, got {other:?}"),
        };
        assert!(
            text.contains(sealed),
            "read_memo must return the contents of the file at the given path: {text}"
        );
    }

    #[test]
    fn unknown_agent_is_not_fabricated() {
        assert_eq!(clearance_for("Vega"), Some("TR-7741-Q"));
        assert_eq!(clearance_for("  vega "), Some("TR-7741-Q"));
        assert_eq!(clearance_for("Nobody"), None);
    }
}
