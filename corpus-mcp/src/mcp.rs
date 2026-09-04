// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP over stdio — newline-delimited JSON-RPC 2.0, the framing every MCP
//! client speaks to a local server. Modeled on `sovereign-server`'s
//! `routes_mcp.rs` and the reference demo server in `sovereign-cli-llm`
//! (`mcp_demo_server.rs`); the envelope types are `oicp_types::jsonrpc`.
//!
//! stdout carries ONLY responses. Everything else — degradations, tracing —
//! goes to stderr, or a client's parser breaks on the first log line.

use anyhow::Result;
use oicp_types::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::tools::Server;

const PROTOCOL_VERSION: &str = "2024-11-05";

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
    if req.method.starts_with("notifications/") {
        return None;
    }
    let id = req.id.clone().unwrap_or(Value::Null);
    let params = req.params.unwrap_or(Value::Null);
    let response = match req.method.as_str() {
        "initialize" => JsonRpcResponse::result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "corpus-mcp", "version": env!("CARGO_PKG_VERSION") },
                "instructions": server.instructions(),
            }),
        ),
        "ping" => JsonRpcResponse::result(id, json!({})),
        "tools/list" => JsonRpcResponse::result(id, json!({ "tools": server.tool_list() })),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match server.call(name, &args).await {
                Ok(outcome) => {
                    let mut result = json!({
                        "content": [ { "type": "text", "text": outcome.text } ],
                        "isError": outcome.is_error,
                    });
                    if let Some(structured) = outcome.structured {
                        result["structuredContent"] = structured;
                    }
                    JsonRpcResponse::result(id, result)
                }
                Err(e) => JsonRpcResponse::error(id, -32601, e.to_string()),
            }
        }
        other => JsonRpcResponse::error(id, -32601, format!("method not found: {other}")),
    };
    Some(response)
}
