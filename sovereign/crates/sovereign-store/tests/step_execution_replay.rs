// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end proof of the step-execution replay-safety invariant (#4):
//! a `NonIdempotent` tool's side-effect runs **at most once** across a
//! crash/resume or a replan — never silently duplicated.
//!
//! Both tests drive the real `Executor` over an in-memory store whose
//! `StepExecutionStore` is the production impl, so the durable attempt
//! ledger is exercised, not stubbed.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::Result;
use sovereign_core::executor::{idempotency_key, AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::skills::SkillRegistry;
use sovereign_core::traits::{InferenceProvider, StepExecutionStore, TaskStore, Tool};
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Effect, ExecutionStatus, Idempotency, Latency,
    Permission, Plan, ProviderCapabilities, Scope, Step, StepExecution, StepKind, StepOutput, Task,
    TaskStatus, ToolContext, ToolDescriptor,
};
use sovereign_store::memory::InMemoryStateStore;

const TOOL_ID: &str = "send_email";

/// A tool-only plan never invokes inference; every method panics so a stray
/// call is loud rather than silently passing the test.
struct NeverInference;

#[async_trait]
impl InferenceProvider for NeverInference {
    async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
        unimplemented!("tool-only plan must not call inference")
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
        unimplemented!("tool-only plan must not call inference")
    }
    async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
        unimplemented!("tool-only plan must not embed")
    }
    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!("tool-only plan must not query capabilities")
    }
}

/// A `NonIdempotent` "send email" tool that counts how many times its
/// side-effect actually fired.
struct CountingTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: TOOL_ID.to_string(),
            name: TOOL_ID.to_string(),
            description: "sends an email (non-idempotent)".to_string(),
            parameters: serde_json::json!({}),
            examples: vec![],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Fast,
            scope: Scope::External,
            output_schema: None,
        }
    }
    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }
    async fn execute(&self, _params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(StepOutput::Text("email sent".to_string()))
    }
}

fn email_params() -> serde_json::Value {
    serde_json::json!({ "to": "alice@example.com", "body": "hi" })
}

fn one_tool_plan() -> Plan {
    Plan {
        id: "p1".to_string(),
        goal: "send the email".to_string(),
        steps: vec![Step {
            id: 0,
            description: "send email".to_string(),
            kind: StepKind::Tool {
                tool_id: TOOL_ID.to_string(),
                params: email_params(),
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    }
}

fn make_task(plan: &Plan) -> Task {
    Task {
        id: "task-1".to_string(),
        conversation_id: "conv-1".to_string(),
        goal: "send the email".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: vec![],
        created_at: 0,
        updated_at: 0,
        version: 0,
    }
}

fn build_executor(store: Arc<InMemoryStateStore>, calls: Arc<AtomicUsize>) -> Executor {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CountingTool { calls }));
    Executor::new(
        Arc::new(NeverInference),
        Arc::new(registry),
        store,
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    )
}

/// Replan dedup: `complex_task` re-runs a replanned plan from an *empty*
/// completed-set. A `NonIdempotent` action that already completed must not
/// fire its side-effect a second time.
#[tokio::test]
async fn completed_action_is_not_reexecuted_on_replan() {
    let store = Arc::new(InMemoryStateStore::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = build_executor(store.clone(), calls.clone());
    let plan = one_tool_plan();
    let task = make_task(&plan);
    store.save_task(&task).await.unwrap();

    // First run: the side-effect fires once and is recorded `Completed`.
    let mut ctx1 = TaskContext {
        task: task.clone(),
        completed: HashMap::new(),
    };
    let r1 = executor.run(&plan, &mut ctx1).await.unwrap();
    assert!(r1.error.is_none(), "first run should succeed");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "side-effect fires once on the first run"
    );

    // Second run from a fresh (empty) context — exactly what a replan does.
    let mut ctx2 = TaskContext {
        task: task.clone(),
        completed: HashMap::new(),
    };
    let r2 = executor.run(&plan, &mut ctx2).await.unwrap();
    assert!(r2.error.is_none(), "replan run should not error");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a completed NonIdempotent action must not re-execute on a replan"
    );
}

/// Crash dedup: a prior attempt began the side-effect and crashed before
/// recording completion (a `Started`-but-not-`Completed` ledger row). On
/// resume the executor must halt and surface, never blind-replay.
#[tokio::test]
async fn in_flight_action_halts_instead_of_replaying() {
    let store = Arc::new(InMemoryStateStore::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = build_executor(store.clone(), calls.clone());
    let plan = one_tool_plan();
    let task = make_task(&plan);
    store.save_task(&task).await.unwrap();

    // Simulate the crash gap: a `Started` row whose key matches what the
    // executor will compute for this exact action, with no completion.
    let key = idempotency_key(&task.id, TOOL_ID, &email_params());
    store
        .record_started(&StepExecution {
            id: "prior-attempt".to_string(),
            task_id: task.id.clone(),
            step_id: 0,
            tool_id: TOOL_ID.to_string(),
            status: ExecutionStatus::Started,
            idempotency_key: key,
            summary: None,
            anomalies: None,
            started_at: 1,
            ended_at: None,
        })
        .await
        .unwrap();

    let mut ctx = TaskContext {
        task: task.clone(),
        completed: HashMap::new(),
    };
    let result = executor.run(&plan, &mut ctx).await.unwrap();

    assert!(
        result.error.is_some(),
        "an in-flight NonIdempotent action must halt the task, not replay it"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the side-effect must NOT fire a second time after a crash"
    );
}
