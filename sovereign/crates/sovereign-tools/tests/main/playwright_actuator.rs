// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live proof that Sovereign drives a *real* browser through the production
//! MCP path (#1 — heterogeneous-app actuation PoC).
//!
//! Ignored by default because it needs `npx @playwright/mcp` and an installed
//! Chromium — CI stays browser-free. Run it on a box that has both:
//!
//! ```text
//! cargo test -p sovereign-tools --test main playwright_actuator -- --ignored --nocapture
//! ```
//!
//! What it proves end to end, against real headless Chromium:
//!   1. `McpServerManager::from_config` (the exact path `load_from_setup_config`
//!      uses) spawns `@playwright/mcp` over stdio and registers its tools.
//!   2. The effect/idempotency classifier tags the real tool metadata
//!      correctly — `browser_click` Write/NonIdempotent (so it picks up the
//!      approval gate + #4 replay ledger), `browser_snapshot` Read/Idempotent.
//!   3. A single browser session survives multiple tool calls (navigate →
//!      snapshot → type+submit), and the form submission actually reaches the
//!      network: the fixture server records the POSTed value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;

use sovereign_core::mcp_config::{McpServerConfig, McpTransportConfig};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::types::{Effect, Idempotency, StepOutput, ToolContext};
use sovereign_tools::mcp::McpServerManager;

const FORM_HTML: &str = r#"<!doctype html>
<html><body>
  <h1>Sovereign Actuator Fixture</h1>
  <form method="POST" action="/submit">
    <input name="message" id="message" />
    <button type="submit" id="send">Send</button>
  </form>
</body></html>"#;

fn tool_ctx() -> ToolContext {
    ToolContext {
        conversation_id: "actuator-test".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
        turn_index: 0,
        ..Default::default()
    }
}

fn text_of(output: &StepOutput) -> String {
    match output {
        StepOutput::Text(t) => t.clone(),
        StepOutput::Json(v) => v.to_string(),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires `npx @playwright/mcp` + an installed Chromium"]
async fn sovereign_drives_real_playwright_browser() {
    // 1. Local fixture: GET / serves a form; POST /submit records the body.
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/", get(|| async { Html(FORM_HTML) }))
        .route(
            "/submit",
            post({
                let captured = Arc::clone(&captured);
                move |body: String| async move {
                    *captured.lock().unwrap() = Some(body);
                    "ok"
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");

    // 2. Register the real @playwright/mcp server via the production path.
    let config = McpServerConfig {
        name: "playwright".to_string(),
        description: None,
        enabled: true,
        transport: McpTransportConfig::Stdio {
            command: "npx".to_string(),
            args: [
                "-y",
                "@playwright/mcp@latest",
                "--headless",
                "--no-sandbox",
                "--isolated",
                // Use Playwright's bundled Chromium, not the system "chrome"
                // channel (which @playwright/mcp defaults to and which isn't
                // installed here).
                "--browser",
                "chromium",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            env: HashMap::new(),
        },
        global: true,
    };
    let mut registry = ToolRegistry::new();
    let _manager = McpServerManager::from_config(&[config], &mut registry).await;

    // 3. Classification on the real descriptors — the Phase 1.5 parity fix.
    let click = registry
        .get("mcp_playwright_browser_click")
        .expect("playwright tools should be registered (is npx/chromium available?)");
    assert_eq!(
        click.descriptor().idempotency,
        Idempotency::NonIdempotent,
        "browser_click must be NonIdempotent so the approval gate + replay ledger engage"
    );
    let snapshot_tool = registry.get("mcp_playwright_browser_snapshot").unwrap();
    assert_eq!(
        snapshot_tool.descriptor().effect,
        Effect::Read,
        "browser_snapshot is a read and must not trip the write/ledger path"
    );

    let ctx = tool_ctx();
    let call = |id: &str, params: serde_json::Value| {
        let id = id.to_string();
        let ctx = &ctx;
        let reg = &registry;
        async move {
            reg.get(&id)
                .unwrap()
                .execute(&params, ctx)
                .await
                .unwrap_or_else(|e| panic!("tool {id} failed: {e}"))
        }
    };

    // 4. Drive the browser — one session across all three calls.
    call("mcp_playwright_browser_navigate", json!({ "url": base })).await;

    let snapshot = call("mcp_playwright_browser_snapshot", json!({})).await;
    assert!(
        text_of(&snapshot).contains("Sovereign Actuator Fixture"),
        "snapshot should observe the fixture page; got: {}",
        text_of(&snapshot)
    );

    // Type into the field and press Enter — a real, non-idempotent write that
    // submits the form. `target` accepts a unique locator, so no ref parsing.
    call(
        "mcp_playwright_browser_type",
        json!({
            "element": "the message input",
            "target": "#message",
            "text": "hello-from-sovereign",
            "submit": true
        }),
    )
    .await;

    // 5. The submission must have reached the network.
    let mut got = None;
    for _ in 0..50 {
        if let Some(body) = captured.lock().unwrap().clone() {
            got = Some(body);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let body = got.expect("fixture server never received the form POST");
    assert!(
        body.contains("message=hello-from-sovereign"),
        "server received the submitted value; got body: {body:?}"
    );
}
