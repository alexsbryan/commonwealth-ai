// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal client for the daemon's MCP tool surface.
//!
//! Exists for the coordination tools (`declare_scope`,
//! `release_scope`, `work_in_flight`): they are only CORRECT against
//! the daemon's work-atlas store — the store peers read, gossip runs
//! off, and CodeWatcher observations land in. The CLI's in-process
//! registry writes a repo-local `mesh.db` no other process reads, so
//! a claim declared there is invisible to everyone (root-caused
//! 2026-07-31). CLI surfaces therefore call the daemon FIRST via this
//! client and fall back to the local store — loudly — only when the
//! daemon is unreachable.

use std::time::Duration;

use serde_json::{json, Value};

use sovereign_contracts::setup_config::client_daemon_base;

/// Why a daemon tool call did not return a payload. The distinction
/// is load-bearing: `Unreachable` means the caller MAY fall back to
/// its local path; `Tool` means the daemon answered and the call
/// FAILED — falling back would silently retry a rejected write
/// against a different store.
#[derive(Debug)]
pub enum DaemonCallError {
    /// No daemon answered (connect/timeout/non-2xx/protocol shape).
    Unreachable(String),
    /// The daemon executed the tool and reported an error.
    Tool(String),
}

impl std::fmt::Display for DaemonCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonCallError::Unreachable(e) => write!(f, "daemon unreachable: {e}"),
            DaemonCallError::Tool(e) => write!(f, "{e}"),
        }
    }
}

/// Call one MCP tool on the local daemon and return its JSON payload.
///
/// Uses the same `/mcp/message` JSON-RPC shape the session-boot brief
/// uses. The 2s ceiling keeps a downed daemon from stalling the CLI:
/// these are local-socket Fast-latency tools; anything slower than
/// that IS unreachable for coordination purposes.
pub async fn daemon_tool_call(tool: &str, arguments: Value) -> Result<Value, DaemonCallError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| DaemonCallError::Unreachable(e.to_string()))?;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    });
    let resp = client
        .post(format!("{}/mcp/message", client_daemon_base()))
        .json(&body)
        .send()
        .await
        .map_err(|e| DaemonCallError::Unreachable(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(DaemonCallError::Unreachable(format!(
            "HTTP {}",
            resp.status()
        )));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| DaemonCallError::Unreachable(e.to_string()))?;
    if let Some(err) = v.get("error") {
        return Err(DaemonCallError::Tool(
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or("MCP error")
                .to_string(),
        ));
    }
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .ok_or_else(|| DaemonCallError::Unreachable("no content in MCP response".into()))?;
    if v["result"]["isError"].as_bool() == Some(true) {
        return Err(DaemonCallError::Tool(text.to_string()));
    }
    // Tool payloads are JSON rendered to text; fall back to the raw
    // string for tools that return prose.
    Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
}

/// Liveness probe for an MCP endpoint: POST an `initialize` request and report
/// whether it answered 2xx. Two seconds, then `false` — this runs at the tail
/// of `project init`, where a slow answer and a dead server are the same
/// outcome for the user.
///
/// Takes a full URL rather than a port because callers probe both the
/// daemon's `/mcp` and a project-local server on a non-default port.
///
/// Note this is deliberately NOT `/healthz` — the daemon has no such route and
/// returns 404 for it.
pub async fn check_mcp_server(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let init_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });

    client
        .post(url)
        .json(&init_body)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}
