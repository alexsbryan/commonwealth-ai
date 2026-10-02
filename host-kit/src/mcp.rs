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
use std::pin::Pin;

use oicp_types::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use oicp_types::mcp::{negotiate_mcp_protocol_version, McpMethod};
// What one tool run produced is MCP wire vocabulary (`CallToolResult`), so it
// lives beside the method set in oicp-types; this is its historical path.
pub use oicp_types::mcp::{CallAudit, ToolOutcome};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(feature = "http")]
pub mod http;

/// What one request carries besides its JSON-RPC body: the transport's view
/// of who asked. Stdio has none of it, so there it is the default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpRequestContext {
    /// The `X-Agent-Session` header the agent sent, if it sent one.
    pub agent_session: Option<String>,
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
    fn call(
        &self,
        name: &str,
        args: &Value,
        ctx: &McpRequestContext,
    ) -> impl Future<Output = Option<ToolOutcome>> + Send;
}

/// The call-log port: told about every call that reached a tool. `()` records
/// nothing.
pub trait McpCallLog {
    /// Record one finished call.
    fn record(&self, tool: &str, outcome: &ToolOutcome, ctx: &McpRequestContext);
}

impl McpCallLog for () {
    fn record(&self, _tool: &str, _outcome: &ToolOutcome, _ctx: &McpRequestContext) {}
}

/// One program's tools, mounted on another program's `/mcp` (phase-b
/// pb-code-daemon-exit: the stock binary serves code's tools on svrn's one
/// `/mcp`, phase-b-33). Object-safe, so the mounting server names no type of
/// the mounted program. [`McpDispatcher`] is one: the mounted program keeps
/// its own tool host and call log, so its calls run and log the same way
/// whether it serves alone or mounted.
pub trait McpMountedTools: Send + Sync {
    /// The `tools/list` entries, exactly as they go on the wire.
    fn list(&self) -> Value;
    /// Run one tool and record it in the mounted program's call log. `None`
    /// means the tool is not this program's; nothing is logged then.
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        ctx: &'a McpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Option<ToolOutcome>> + Send + 'a>>;
}

impl<H, L> McpMountedTools for McpDispatcher<H, L>
where
    H: McpToolHost + Send + Sync,
    L: McpCallLog + Send + Sync,
{
    fn list(&self) -> Value {
        self.host.list()
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        ctx: &'a McpRequestContext,
    ) -> Pin<Box<dyn Future<Output = Option<ToolOutcome>> + Send + 'a>> {
        Box::pin(self.run_tool(name, args, ctx))
    }
}

/// Answers one JSON-RPC request. [`McpDispatcher`] is the kit's; a host with
/// its own method dispatch implements this to reuse the batch handling
/// ([`dispatch_body`]) and the framings.
pub trait McpRequestHandler: Send + Sync {
    /// `None` for a notification, which gets no reply.
    fn handle(
        &self,
        req: JsonRpcRequest,
        ctx: &McpRequestContext,
    ) -> impl Future<Output = Option<JsonRpcResponse>> + Send;
}

/// Answer one JSON-RPC body: a single request or a batch. `None` when
/// nothing needs a reply (a notification, or a batch of them). An empty
/// batch and an entry that is not a request are -32600.
pub async fn dispatch_body<H: McpRequestHandler + ?Sized>(
    handler: &H,
    body: Value,
    ctx: &McpRequestContext,
) -> Option<Value> {
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
                    Ok(req) => responses.extend(handler.handle(req, ctx).await),
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
            Ok(req) => handler.handle(req, ctx).await.map(|r| json!(r)),
            Err(_) => Some(json!(JsonRpcResponse::error(
                Value::Null,
                -32600,
                "invalid request"
            ))),
        },
    }
}

/// One MCP server: a tool host and a call log behind the protocol.
pub struct McpDispatcher<H, L = ()> {
    server_name: String,
    server_version: String,
    host: H,
    log: L,
    list_changed: bool,
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
            list_changed: false,
        }
    }

    /// Advertise `tools.listChanged`: the host pushes
    /// `notifications/tools/list_changed` when its list changes (the HTTP
    /// framing's notifier carries it).
    pub fn list_changed(mut self, yes: bool) -> Self {
        self.list_changed = yes;
        self
    }

    /// Answer one request. `None` for a notification (a request without an
    /// id, whatever its method), which gets no reply by contract (JSON-RPC
    /// 2.0 §4.1).
    pub async fn dispatch(
        &self,
        req: JsonRpcRequest,
        ctx: &McpRequestContext,
    ) -> Option<JsonRpcResponse> {
        let Some(id) = req.id else {
            tracing::debug!(method = %req.method, "mcp: notification received");
            return None;
        };
        let params = req.params.unwrap_or(Value::Null);
        let response = match McpMethod::parse(&req.method) {
            Some(McpMethod::Initialize) => {
                let mut result = json!({
                    "protocolVersion": negotiate_mcp_protocol_version(Some(&params)),
                    "capabilities": { "tools": { "listChanged": self.list_changed } },
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
                match self.run_tool(name, &args, ctx).await {
                    Some(outcome) => {
                        tracing::debug!(tool = name, is_error = outcome.is_error, "mcp: tool ran");
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

    /// Run one tool through the host and record it in the log: the one
    /// `tools/call` path, served alone or mounted ([`McpMountedTools`]).
    async fn run_tool(
        &self,
        name: &str,
        args: &Value,
        ctx: &McpRequestContext,
    ) -> Option<ToolOutcome> {
        let outcome = self.host.call(name, args, ctx).await?;
        self.log.record(name, &outcome, ctx);
        Some(outcome)
    }

    /// Answer one JSON-RPC body through [`dispatch_body`].
    pub async fn dispatch_body(&self, body: Value, ctx: &McpRequestContext) -> Option<Value>
    where
        H: Send + Sync,
        L: Send + Sync,
    {
        dispatch_body(self, body, ctx).await
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
                Ok(req) => match self.dispatch(req, &McpRequestContext::default()).await {
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

impl<H, L> McpRequestHandler for McpDispatcher<H, L>
where
    H: McpToolHost + Send + Sync,
    L: McpCallLog + Send + Sync,
{
    fn handle(
        &self,
        req: JsonRpcRequest,
        ctx: &McpRequestContext,
    ) -> impl Future<Output = Option<JsonRpcResponse>> + Send {
        self.dispatch(req, ctx)
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
        async fn call(
            &self,
            name: &str,
            args: &Value,
            _ctx: &McpRequestContext,
        ) -> Option<ToolOutcome> {
            match name {
                "echo" => Some(ToolOutcome {
                    text: args["text"].as_str().unwrap_or("").to_string(),
                    is_error: false,
                    structured: Some(json!({ "echoed": true })),
                    audit: None,
                }),
                "refuse" => Some(ToolOutcome {
                    text: "no".into(),
                    is_error: true,
                    structured: None,
                    audit: None,
                }),
                _ => None,
            }
        }
    }

    #[derive(Default)]
    struct RecordingLog(Mutex<Vec<(String, bool)>>);

    impl McpCallLog for &RecordingLog {
        fn record(&self, tool: &str, outcome: &ToolOutcome, _ctx: &McpRequestContext) {
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

    /// Mounted behind `dyn McpMountedTools`, a dispatcher lists its host's
    /// tools, runs one through its host and logs it in its own log, and
    /// answers `None` for a tool it does not have, logging nothing.
    #[tokio::test]
    async fn a_mounted_dispatcher_runs_and_logs_its_own_tools() {
        let log = RecordingLog::default();
        let dispatcher = McpDispatcher::new("fake", "0", FakeHost, &log);
        let mounted: &dyn McpMountedTools = &dispatcher;
        let ctx = McpRequestContext::default();
        assert_eq!(mounted.list()[0]["name"], "echo");
        let ran = mounted.call("echo", &json!({ "text": "hi" }), &ctx).await;
        assert_eq!(ran.map(|o| o.text), Some("hi".to_string()));
        assert!(mounted.call("nope", &json!({}), &ctx).await.is_none());
        assert_eq!(*log.0.lock().unwrap(), vec![("echo".to_string(), false)]);
    }

    /// Batches: notifications inside one are dropped, a non-request entry is
    /// -32600, an empty batch is -32600, an all-notification batch needs no
    /// reply, and a single body answers like `dispatch`.
    #[tokio::test]
    async fn dispatch_body_handles_batches_and_single_bodies() {
        let dispatcher = McpDispatcher::new("fake", "0", FakeHost, ());
        let ctx = McpRequestContext::default();
        let batch = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "no": "method" },
        ]);
        let replies = dispatcher.dispatch_body(batch, &ctx).await.unwrap();
        let replies = replies.as_array().unwrap();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[0]["result"], json!({}));
        assert_eq!(replies[1]["error"]["code"], -32600);

        let empty = dispatcher.dispatch_body(json!([]), &ctx).await.unwrap();
        assert_eq!(empty["error"]["message"], "empty batch");

        let quiet = json!([{ "jsonrpc": "2.0", "method": "notifications/initialized" }]);
        assert_eq!(dispatcher.dispatch_body(quiet, &ctx).await, None);

        let single = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list" });
        let reply = dispatcher.dispatch_body(single, &ctx).await.unwrap();
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["result"]["tools"][0]["name"], "echo");
    }
}
