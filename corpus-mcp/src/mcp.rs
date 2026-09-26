// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP over stdio — newline-delimited JSON-RPC 2.0, the framing every MCP
//! client speaks to a local server. Modeled on the reference demo server in
//! `sovereign-cli-llm` (`mcp_demo_server.rs`); the envelope types are
//! `oicp_types::jsonrpc`.
//!
//! stdout carries ONLY responses. Everything else — degradations, tracing —
//! goes to stderr, or a client's parser breaks on the first log line.

use anyhow::Result;
use oicp_types::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use oicp_types::mcp::{negotiate_mcp_protocol_version, McpMethod};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::tools::Server;

pub async fn serve_stdio(server: Server) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    eprintln!("corpus-mcp: ready on stdio");
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(req) => match dispatch(&server, req).await {
                Some(r) => r,
                None => continue, // a notification: no reply by contract
            },
            Err(e) => JsonRpcResponse::error(Value::Null, -32700, format!("parse error: {e}")),
        };
        out.write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        out.write_all(b"\n").await?;
        out.flush().await?;
    }
    Ok(())
}

async fn dispatch(server: &Server, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    // A request without an id is a notification, whatever its method: no
    // reply by contract (JSON-RPC 2.0 §4.1).
    let Some(id) = req.id else {
        tracing::debug!(method = %req.method, "corpus-mcp: notification received");
        return None;
    };
    let params = req.params.unwrap_or(Value::Null);
    let response = match McpMethod::parse(&req.method) {
        Some(McpMethod::Initialize) => JsonRpcResponse::result(
            id,
            json!({
                "protocolVersion": negotiate_mcp_protocol_version(Some(&params)),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "corpus-mcp", "version": env!("CARGO_PKG_VERSION") },
                "instructions": server.instructions(),
            }),
        ),
        Some(McpMethod::Ping) => JsonRpcResponse::result(id, json!({})),
        Some(McpMethod::ToolsList) => {
            JsonRpcResponse::result(id, json!({ "tools": server.tool_list() }))
        }
        Some(McpMethod::ToolsCall) => {
            let name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match server.call(name, &args).await {
                Some(outcome) => {
                    let mut result = json!({
                        "content": [ { "type": "text", "text": outcome.text } ],
                        "isError": outcome.is_error,
                    });
                    if let Some(structured) = outcome.structured {
                        result["structuredContent"] = structured;
                    }
                    JsonRpcResponse::result(id, result)
                }
                None => JsonRpcResponse::error(id, -32601, format!("tool not found: {name}")),
            }
        }
        None => JsonRpcResponse::error(id, -32601, format!("method not found: {}", req.method)),
    };
    Some(response)
}
