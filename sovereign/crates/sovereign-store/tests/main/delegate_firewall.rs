// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hermetic proof of the §5.2 context firewall (#2): a `Delegate` worker's
//! raw tool observations (a page DOM, a sheet's cells) stay inside the worker;
//! only the typed contract flows back to the orchestrator.
//!
//! A scripted mock inference drives the worker loop deterministically: read a
//! tool that returns a huge blob, report findings, then emit the contract. The
//! test asserts the orchestrator's step output is the small typed contract and
//! does NOT contain the blob.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::Result;
use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::skills::SkillRegistry;
use sovereign_core::traits::{InferenceProvider, Tool};
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Effect, Idempotency, Latency, Permission, Plan,
    ProviderCapabilities, Scope, Step, StepKind, StepOutput, Task, TaskStatus, ToolContext,
    ToolDescriptor,
};
use sovereign_store::memory::InMemoryStateStore;

const BLOB: &str = "SECRET_DOM_BLOB <html><body><div role=heading>Hello World</div>\
    <div>…thousands of nodes of raw page DOM the orchestrator must never see…</div>\
    </body></html>";

/// Inference that replays a fixed script of responses in order, ignoring the
/// prompt — enough to drive the worker loop deterministically.
struct ScriptedInference {
    responses: Vec<String>,
    idx: AtomicUsize,
}

#[async_trait]
impl InferenceProvider for ScriptedInference {
    async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
        let i = self.idx.fetch_add(1, Ordering::SeqCst);
        let text = self.responses.get(i).cloned().unwrap_or_default();
        Ok(CompletionResponse {
            text,
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
        unimplemented!("worker uses non-streaming complete")
    }
    async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
        unimplemented!()
    }
    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!()
    }
}

/// A read tool that returns a huge blob — the "page DOM" the worker wades
/// through and the orchestrator must never receive.
struct BlobTool;

#[async_trait]
impl Tool for BlobTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "read_page".to_string(),
            name: "read_page".to_string(),
            description: "returns the full page".to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::External,
            output_schema: None,
        }
    }
    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }
    async fn execute(&self, _p: &serde_json::Value, _c: &ToolContext) -> Result<StepOutput> {
        Ok(StepOutput::Text(BLOB.to_string()))
    }
}

#[tokio::test]
async fn delegate_worker_firewalls_raw_observations() {
    // Worker script: (1) call read_page; (2) report findings (no tool call →
    // loop breaks); (3) the structured-synthesis pass emits the contract.
    let responses = vec![
        r#"I'll read the page. <tool_call>{"name":"read_page","arguments":{}}</tool_call>"#
            .to_string(),
        "The page heading is Hello World.".to_string(),
        r#"{"heading":"Hello World","anomalies":""}"#.to_string(),
    ];
    let inference = Arc::new(ScriptedInference {
        responses,
        idx: AtomicUsize::new(0),
    });
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(BlobTool));
    let executor = Executor::new(
        inference,
        Arc::new(registry),
        Arc::new(InMemoryStateStore::new()),
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );

    let plan = Plan {
        id: "p".to_string(),
        goal: "extract the heading".to_string(),
        steps: vec![Step {
            id: 0,
            description: "extract heading via a firewalled worker".to_string(),
            kind: StepKind::Delegate {
                goal: "Find the page heading.".to_string(),
                tools: vec!["read_page".to_string()],
                return_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "heading": { "type": "string" } },
                    "required": ["heading"],
                }),
                max_iterations: 4,
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
        goal: "extract the heading".to_string(),
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

    let output = result.completed.get(&0).expect("delegate step output");
    let json = match output {
        StepOutput::Json(v) => v,
        other => panic!("expected a typed JSON contract, got {other:?}"),
    };

    // The contract carries the extracted field + the always-present anomalies
    // channel — and nothing else the orchestrator must reason over.
    assert_eq!(json["heading"], serde_json::json!("Hello World"));
    assert!(
        json.get("anomalies").is_some(),
        "the anomalies surprises-channel must always be present"
    );

    // THE FIREWALL: the raw blob the worker read must not appear anywhere in
    // what reached the orchestrator.
    let orchestrator_view = serde_json::to_string(json).unwrap();
    assert!(
        !orchestrator_view.contains("SECRET_DOM_BLOB"),
        "raw observation leaked past the firewall into the orchestrator: {orchestrator_view}"
    );
}
