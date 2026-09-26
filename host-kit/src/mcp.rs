// SPDX-License-Identifier: AGPL-3.0-or-later
//! The MCP dispatcher: the protocol half every MCP host shares (phase-b
//! pb-mcp, decision phase-b-13). It owns method dispatch, `initialize`,
//! notifications, batches and the stdio framing. What the tools ARE is the
//! program's, supplied through two ports: [`McpToolHost`] (instructions, the
//! tool list, running one tool) and [`McpCallLog`] (what to record about a
//! call). The kit names no program's vocabulary, so a registry, a store or a
//! pattern matcher is the port implementation's business, never this file's.
//!
//! The wire vocabulary (the method set, version negotiation, the JSON-RPC
//! envelope) is `oicp-types`'; this module only dispatches over it.

use std::future::Future;

use oicp_types::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use oicp_types::mcp::{negotiate_mcp_protocol_version, McpMethod};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// What one tool run produced. A tool that ran and failed is an outcome with
/// `is_error` set, which the agent reads and can recover from; it is never a
/// JSON-RPC error, which a client treats as a broken transport.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// The text the agent reads.
    pub text: String,
    /// The tool said no.
    pub is_error: bool,
    /// Optional machine-readable result (`structuredContent`).
    pub structured: Option<Value>,
}

impl ToolOutcome {
    /// The MCP `CallToolResult` this outcome is on the wire.
    pub fn into_call_result(self) -> Value {
        let mut result = json!({
            "content": [ { "type": "text", "text": self.text } ],
            "isError": self.is_error,
        });
        if let Some(structured) = self.structured {
            result["structuredContent"] = structured;
        }
        result
    }
}

/// The tool-host port: what a program supplies so the dispatcher can serve
/// its tools.
pub trait McpToolHost {
    /// Free text for `initialize`'s `instructions`, if the host has any.
    fn instructions(&self) -> Option<String>;
    /// The `tools/list` array, exactly as it goes on the wire.
    fn list(&self) -> Value;
    /// Run one tool. `None` means no tool has that name (-32601); a tool that
    /// ran and failed is `Some` with `is_error` set.
    fn call(&self, name: &str, args: &Value) -> impl Future<Output = Option<ToolOutcome>>;
}

/// The call-log port: told about every call that reached a tool. `()` records
/// nothing.
pub trait McpCallLog {
    /// Record one finished call.
    fn record(&self, tool: &str, outcome: &ToolOutcome);
}

impl McpCallLog for () {
    fn record(&self, _tool: &str, _outcome: &ToolOutcome) {}
}

/// One MCP server: a tool host and a call log behind the protocol.
pub struct McpDispatcher<H, L = ()> {
    server_name: String,
    server_version: String,
    host: H,
    log: L,
}

impl<H: McpToolHost, L: McpCallLog> McpDispatcher<H, L> {
    /// `server_name` and `server_version` go out as `initialize`'s
    /// `serverInfo`.
    pub fn new(
        server_name: impl Into<String>,
        server_version: impl Into<String>,
        host: H,
        log: L,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            server_version: server_version.into(),
            host,
            log,
        }
    }

    /// Answer one request. `None` for a notification (a request without an
    /// id, whatever its method), which gets no reply by contract (JSON-RPC
    /// 2.0 §4.1).
    pub async fn dispatch(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let Some(id) = req.id else {
            tracing::debug!(method = %req.method, "mcp: notification received");
            return None;
        };
        let params = req.params.unwrap_or(Value::Null);
        let response = match McpMethod::parse(&req.method) {
            Some(McpMethod::Initialize) => {
                let mut result = json!({
                    "protocolVersion": negotiate_mcp_protocol_version(Some(&params)),
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": self.server_name, "version": self.server_version },
                });
                if let Some(instructions) = self.host.instructions() {
                    result["instructions"] = Value::String(instructions);
                }
                JsonRpcResponse::result(id, result)
            }
            Some(McpMethod::Ping) => JsonRpcResponse::result(id, json!({})),
            Some(McpMethod::ToolsList) => {
                JsonRpcResponse::result(id, json!({ "tools": self.host.list() }))
            }
            Some(McpMethod::ToolsCall) => {
                let name = params["name"].as_str().unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.host.call(name, &args).await {
                    Some(outcome) => {
                        tracing::debug!(tool = name, is_error = outcome.is_error, "mcp: tool ran");
                        self.log.record(name, &outcome);
                        JsonRpcResponse::result(id, outcome.into_call_result())
                    }
                    None => {
                        tracing::debug!(tool = name, "mcp: tool not found");
                        JsonRpcResponse::error(id, -32601, format!("tool not found: {name}"))
                    }
                }
            }
            None => {
                tracing::debug!(method = %req.method, "mcp: method not found");
                JsonRpcResponse::error(id, -32601, format!("method not found: {}", req.method))
            }
        };
        Some(response)
    }

    /// Answer one JSON-RPC body: a single request or a batch. `None` when
    /// nothing needs a reply (a notification, or a batch of them). An empty
    /// batch and an entry that is not a request are -32600.
    pub async fn dispatch_body(&self, body: Value) -> Option<Value> {
        match body {
            Value::Array(items) => {
                if items.is_empty() {
                    return Some(json!(JsonRpcResponse::error(
                        Value::Null,
                        -32600,
                        "empty batch"
                    )));
                }
                let mut responses = Vec::new();
                for item in items {
                    match serde_json::from_value::<JsonRpcRequest>(item) {
                        Ok(req) => responses.extend(self.dispatch(req).await),
                        Err(_) => responses.push(JsonRpcResponse::error(
                            Value::Null,
                            -32600,
                            "invalid request",
                        )),
                    }
                }
                (!responses.is_empty()).then(|| json!(responses))
            }
            single => match serde_json::from_value::<JsonRpcRequest>(single) {
                Ok(req) => self.dispatch(req).await.map(|r| json!(r)),
                Err(_) => Some(json!(JsonRpcResponse::error(
                    Value::Null,
                    -32600,
                    "invalid request"
                ))),
            },
        }
    }

    /// Serve newline-delimited JSON-RPC 2.0 over stdio until stdin closes.
    /// stdout carries only responses; anything diagnostic belongs on stderr,
    /// or a client's parser breaks on the first log line.
    pub async fn serve_stdio(&self) -> std::io::Result<()> {
        self.serve_lines(
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
        )
        .await
    }

    /// The stdio framing over any line reader and writer: one request per
    /// line, one response per line, blank lines skipped, a line that is not a
    /// request answered -32700.
    pub async fn serve_lines<R, W>(&self, reader: R, mut writer: W) -> std::io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
                Ok(req) => match self.dispatch(req).await {
                    Some(r) => r,
                    None => continue,
                },
                Err(e) => JsonRpcResponse::error(Value::Null, -32700, format!("parse error: {e}")),
            };
            writer
                .write_all(json!(response).to_string().as_bytes())
                .await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A fake host: `echo` returns its `text` argument, `refuse` runs and
    /// says no, anything else does not exist.
    struct FakeHost;

    impl McpToolHost for FakeHost {
        fn instructions(&self) -> Option<String> {
            Some("fake instructions".into())
        }
        fn list(&self) -> Value {
            json!([{ "name": "echo" }, { "name": "refuse" }])
        }
        async fn call(&self, name: &str, args: &Value) -> Option<ToolOutcome> {
            match name {
                "echo" => Some(ToolOutcome {
                    text: args["text"].as_str().unwrap_or("").to_string(),
                    is_error: false,
                    structured: Some(json!({ "echoed": true })),
                }),
                "refuse" => Some(ToolOutcome {
                    text: "no".into(),
                    is_error: true,
                    structured: None,
                }),
                _ => None,
            }
        }
    }

    #[derive(Default)]
    struct RecordingLog(Mutex<Vec<(String, bool)>>);

    impl McpCallLog for &RecordingLog {
        fn record(&self, tool: &str, outcome: &ToolOutcome) {
            self.0
                .lock()
                .unwrap()
                .push((tool.to_string(), outcome.is_error));
        }
    }

    /// The stdio framing end to end through the fake port: `initialize`
    /// negotiates and carries the host's instructions, a notification gets no
    /// line, `tools/list` is the host's list, `tools/call` returns the
    /// outcome (isError and structuredContent included), an unknown tool and
    /// an unknown method are -32601, a garbage line is -32700, and the call
    /// log saw exactly the two calls that reached a tool.
    #[tokio::test]
    async fn stdio_drives_initialize_list_call_and_notification_through_the_port() {
        let log = RecordingLog::default();
        let dispatcher = McpDispatcher::new("fake", "0.0.1", FakeHost, &log);
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"refuse"}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/list"}"#,
            "not json",
        ]
        .join("\n");
        let mut out = Vec::new();
        dispatcher
            .serve_lines(input.as_bytes(), &mut out)
            .await
            .unwrap();
        let replies: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            replies.len(),
            7,
            "the notification gets no line: {replies:?}"
        );

        let init = &replies[0]["result"];
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(init["protocolVersion"], "2025-03-26");
        assert_eq!(init["serverInfo"]["name"], "fake");
        assert_eq!(init["serverInfo"]["version"], "0.0.1");
        assert_eq!(init["instructions"], "fake instructions");

        assert_eq!(replies[1]["id"], 2);
        assert_eq!(replies[1]["result"]["tools"][1]["name"], "refuse");

        let echo = &replies[2]["result"];
        assert_eq!(echo["content"][0]["text"], "hi");
        assert_eq!(echo["isError"], false);
        assert_eq!(echo["structuredContent"]["echoed"], true);

        assert_eq!(replies[3]["result"]["isError"], true);
        assert!(replies[3]["result"].get("structuredContent").is_none());

        assert_eq!(replies[4]["error"]["code"], -32601);
        assert_eq!(replies[4]["error"]["message"], "tool not found: nope");
        assert_eq!(replies[5]["error"]["code"], -32601);
        assert_eq!(
            replies[5]["error"]["message"],
            "method not found: resources/list"
        );
        assert_eq!(replies[6]["error"]["code"], -32700);
        assert_eq!(replies[6]["id"], Value::Null);

        assert_eq!(
            *log.0.lock().unwrap(),
            vec![("echo".to_string(), false), ("refuse".to_string(), true)]
        );
    }

    /// Batches: notifications inside one are dropped, a non-request entry is
    /// -32600, an empty batch is -32600, an all-notification batch needs no
    /// reply, and a single body answers like `dispatch`.
    #[tokio::test]
    async fn dispatch_body_handles_batches_and_single_bodies() {
        let dispatcher = McpDispatcher::new("fake", "0", FakeHost, ());
        let batch = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "no": "method" },
        ]);
        let replies = dispatcher.dispatch_body(batch).await.unwrap();
        let replies = replies.as_array().unwrap();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[0]["result"], json!({}));
        assert_eq!(replies[1]["error"]["code"], -32600);

        let empty = dispatcher.dispatch_body(json!([])).await.unwrap();
        assert_eq!(empty["error"]["message"], "empty batch");

        let quiet = json!([{ "jsonrpc": "2.0", "method": "notifications/initialized" }]);
        assert_eq!(dispatcher.dispatch_body(quiet).await, None);

        let single = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list" });
        let reply = dispatcher.dispatch_body(single).await.unwrap();
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["result"]["tools"][0]["name"], "echo");
    }
}
