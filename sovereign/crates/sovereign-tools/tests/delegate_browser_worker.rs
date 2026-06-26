// SPDX-License-Identifier: AGPL-3.0-or-later
//! The #1 × #2 composition, proven live: a `Delegate` context-firewall worker
//! drives a REAL browser (via `@playwright/mcp`) to extract one field, and the
//! orchestrator receives ONLY the typed contract — never the page DOM.
//!
//! Ignored by default (needs `npx @playwright/mcp` + an installed Chromium):
//!
//! ```text
//! cargo test -p sovereign-tools --test delegate_browser_worker -- --ignored --nocapture
//! ```
//!
//! A scripted mock inference plays the worker's reasoning (navigate → snapshot
//! → report findings → emit contract) so the test is deterministic, but the
//! browser actuation and the snapshot are real. The asserts: the worker really
//! drove the browser (the fixture server saw the GET), the orchestrator's step
//! output is the small `{heading, anomalies}` contract, and the raw
//! accessibility snapshot (`[ref=…]` markers) never reached the orchestrator.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::response::Html;
use axum::routing::get;
use axum::Router;

use sovereign_core::error::Result;
use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::mcp_config::{McpServerConfig, McpTransportConfig};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::skills::SkillRegistry;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Plan, ProviderCapabilities, Step, StepKind, StepOutput,
    Task, TaskStatus,
};
use sovereign_store::memory::InMemoryStateStore;
use sovereign_tools::mcp::McpServerManager;

const FIXTURE_HTML: &str = r#"<!doctype html>
<html><body>
  <h1>Sovereign Actuator Fixture</h1>
  <form method="POST" action="/submit">
    <input name="message" id="message" />
    <button type="submit" id="send">Send</button>
  </form>
</body></html>"#;

struct ScriptedInference {
    responses: Vec<String>,
    idx: AtomicUsize,
}

#[async_trait]
impl InferenceProvider for ScriptedInference {
    async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
        let i = self.idx.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            text: self.responses.get(i).cloned().unwrap_or_default(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "scripted".to_string(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        unimplemented!()
    }
    async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
        unimplemented!()
    }
    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!()
    }
}

#[tokio::test]
#[ignore = "requires `npx @playwright/mcp` + an installed Chromium"]
async fn delegate_worker_drives_real_browser_and_firewalls_the_dom() {
    // 1. Local fixture; count GETs so we can prove the worker really navigated.
    let gets = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/",
        get({
            let gets = Arc::clone(&gets);
            move || async move {
                gets.fetch_add(1, Ordering::SeqCst);
                Html(FIXTURE_HTML)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}/");

    // 2. Register real @playwright/mcp; scope the worker to navigate + snapshot.
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
    assert!(
        registry.get("mcp_playwright_browser_snapshot").is_ok(),
        "playwright tools should be registered (is npx/chromium available?)"
    );

    // 3. Scripted worker reasoning: navigate → snapshot → findings → contract.
    let responses = vec![
        format!(
            r#"Navigating. <tool_call>{{"name":"mcp_playwright_browser_navigate","arguments":{{"url":"{base}"}}}}</tool_call>"#
        ),
        r#"Reading the page. <tool_call>{"name":"mcp_playwright_browser_snapshot","arguments":{}}</tool_call>"#
            .to_string(),
        "The page heading is 'Sovereign Actuator Fixture'.".to_string(),
        r#"{"heading":"Sovereign Actuator Fixture","anomalies":""}"#.to_string(),
    ];
    let inference = Arc::new(ScriptedInference {
        responses,
        idx: AtomicUsize::new(0),
    });

    let executor = Executor::new(
        inference,
        Arc::new(registry),
        Arc::new(InMemoryStateStore::new()),
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );

    // 4. One Delegate step. The orchestrator only ever names the tools and the
    //    contract — it never touches the browser itself.
    let plan = Plan {
        id: "p".to_string(),
        goal: "get the page heading".to_string(),
        steps: vec![Step {
            id: 0,
            description: "extract the heading via a firewalled browser worker".to_string(),
            kind: StepKind::Delegate {
                goal: format!("Open {base}, read the page, and report its main heading."),
                tools: vec![
                    "mcp_playwright_browser_navigate".to_string(),
                    "mcp_playwright_browser_snapshot".to_string(),
                ],
                return_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "heading": { "type": "string" } },
                    "required": ["heading"],
                }),
                max_iterations: 5,
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };
    let task = Task {
        id: "t".to_string(),
        conversation_id: "c".to_string(),
        goal: "get the page heading".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: vec![],
        created_at: 0,
        updated_at: 0,
        version: 0,
    };
    let mut ctx = TaskContext {
        task,
        completed: HashMap::new(),
    };
    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none(), "delegate run should succeed");

    // The worker really drove the browser.
    assert!(
        gets.load(Ordering::SeqCst) >= 1,
        "the worker should have navigated the real browser to the fixture"
    );

    // The orchestrator got the typed contract...
    let json = match result.completed.get(&0).expect("delegate output") {
        StepOutput::Json(v) => v.clone(),
        other => panic!("expected a JSON contract, got {other:?}"),
    };
    assert_eq!(json["heading"], serde_json::json!("Sovereign Actuator Fixture"));
    assert!(json.get("anomalies").is_some());

    // ...and the raw accessibility snapshot never reached it. `[ref=` is a
    // definitive @playwright/mcp snapshot marker; if the firewall leaked the
    // worker transcript, it would appear here.
    let orchestrator_view = serde_json::to_string(&json).unwrap();
    assert!(
        !orchestrator_view.contains("[ref="),
        "the raw page snapshot leaked past the firewall: {orchestrator_view}"
    );
}
