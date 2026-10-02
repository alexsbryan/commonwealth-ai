// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the MCP HTTP+SSE framing — see `http.rs`.

use super::*;
use crate::mcp::{McpDispatcher, McpToolHost, ToolOutcome};
use serde_json::json;
use std::sync::Mutex;

/// `notify_tools_list_changed` delivers the same frame to every subscriber:
/// independent cursors, broadcast fan-out. (Moved from the daemon's
/// mcp_router with the notifier.)
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

    assert_eq!(recv_a, recv_b, "both subscribers see the same payload");
    assert_eq!(recv_a["method"], "notifications/tools/list_changed");
    assert_eq!(recv_a["jsonrpc"], "2.0");
}

/// Publishing with no subscribers is a no-op: no panic, no block.
#[test]
fn notifier_publish_with_no_subscribers_is_noop() {
    McpNotifier::new().notify_tools_list_changed();
}

/// Echoes the agent session the framing read off the request.
struct SessionEcho;

impl McpToolHost for SessionEcho {
    fn instructions(&self) -> Option<String> {
        None
    }
    fn list(&self) -> Value {
        json!([{ "name": "whoami" }])
    }
    async fn call(
        &self,
        name: &str,
        _args: &Value,
        ctx: &McpRequestContext,
    ) -> Option<ToolOutcome> {
        (name == "whoami")
            .then(|| ToolOutcome::answer(ctx.agent_session.clone().unwrap_or_default(), None))
    }
}

#[derive(Default)]
struct Calls(Mutex<Vec<String>>);

impl crate::mcp::McpCallLog for Arc<Calls> {
    fn record(&self, tool: &str, _outcome: &ToolOutcome, ctx: &McpRequestContext) {
        self.0.lock().unwrap().push(format!(
            "{tool}:{}",
            ctx.agent_session.as_deref().unwrap_or("-")
        ));
    }
}

/// Over a real socket: a call carries the `X-Agent-Session` header to the
/// host and the log, `/mcp/message` answers like `/mcp`, a notification is
/// an empty 202, `listChanged` follows the dispatcher's flag, and the SSE
/// stream opens with the `endpoint` event and then carries a pushed
/// `tools/list_changed`.
#[tokio::test]
async fn http_framing_answers_posts_and_streams_notifications() {
    let calls = Arc::new(Calls::default());
    let dispatcher =
        McpDispatcher::new("fake", "0", SessionEcho, Arc::clone(&calls)).list_changed(true);
    let notifier = McpNotifier::new();
    let app = routes(Arc::new(dispatcher), notifier.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::new();

    let init: Value = client
        .post(format!("{base}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);

    let call: Value = client
        .post(format!("{base}/mcp/message"))
        .header("X-Agent-Session", "agent-7")
        .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                       "params": { "name": "whoami" } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(call["result"]["content"][0]["text"], "agent-7");
    assert_eq!(*calls.0.lock().unwrap(), vec!["whoami:agent-7".to_string()]);

    let quiet = client
        .post(format!("{base}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .send()
        .await
        .unwrap();
    assert_eq!(quiet.status(), reqwest::StatusCode::ACCEPTED);
    assert!(quiet.text().await.unwrap().is_empty());

    let mut sse = client.get(format!("{base}/mcp")).send().await.unwrap();
    let mut seen = String::new();
    let mut pushed = false;
    while !seen.contains("tools/list_changed") {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), sse.chunk())
            .await
            .expect("the stream answers within 5s")
            .unwrap()
            .expect("the stream stays open");
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.contains("event: endpoint") && !pushed {
            notifier.notify_tools_list_changed();
            pushed = true;
        }
    }
    assert!(seen.contains("event: endpoint\ndata: /mcp"), "{seen}");
}
