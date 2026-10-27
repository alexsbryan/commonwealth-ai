// SPDX-License-Identifier: AGPL-3.0-or-later
//! The executor's tool loop sends a conversation and reads the reply
//! (es-t2-executor-native): one `Delegate` step with one tool round trip, on
//! the shipped llama-server through `oicp-client`, the router's wire, running
//! Qwen3.5-0.8B from `SOVEREIGN_MODELS_DIR`. Its embedded-engine twin went
//! with that engine (ENGINE_SWAP_PLAN G2).
//!
//! Ignored: it loads a model and boots a server.
//!
//! ```text
//! cargo test -p sovereign-stock --test executor_native_e2e -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use oicp_client::RemoteApiProvider;
use sovereign_contracts::traits::{InferenceProvider, Tool};
use sovereign_contracts::types::{
    Effect, Idempotency, Latency, Permission, Plan, Scope, Step, StepKind, StepOutput, Task,
    TaskStatus, ToolContext, ToolDescriptor,
};
use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::skills::SkillRegistry;
use sovereign_store::memory::InMemoryStateStore;

const MODEL: &str = "Qwen3.5-0.8B-UD-Q6_K_XL.gguf";
const CAPITAL: &str = "Zorbaville";

/// The one tool: what it returns cannot be known without calling it.
struct LookupCapital(Arc<AtomicUsize>);

#[async_trait]
impl Tool for LookupCapital {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "lookup_capital".into(),
            name: "lookup_capital".into(),
            description: "Returns the capital city of a country.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "country": { "type": "string" } },
                "required": ["country"],
            }),
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
    async fn execute(
        &self,
        params: &serde_json::Value,
        _: &ToolContext,
    ) -> sovereign_contracts::Result<StepOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        eprintln!("tool called with {params}");
        Ok(StepOutput::Text(format!(
            "The capital of Freedonia is {CAPITAL}."
        )))
    }
}

/// Run one delegate step on `inference`; the tool's call count and the
/// contract the orchestrator received.
async fn delegate_once(inference: Arc<dyn InferenceProvider>) -> (usize, serde_json::Value) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(LookupCapital(Arc::clone(&calls))));
    let executor = Executor::new(
        inference,
        Arc::new(registry),
        Arc::new(InMemoryStateStore::new()),
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );
    let plan = Plan {
        id: "p".into(),
        goal: "capital".into(),
        steps: vec![Step {
            id: 0,
            description: "look the capital up".into(),
            kind: StepKind::Delegate {
                goal: "What is the capital of Freedonia? Look it up with the tool.".into(),
                tools: vec!["lookup_capital".into()],
                return_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "capital": { "type": "string" } },
                    "required": ["capital"],
                }),
                max_iterations: 3,
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };
    let task = Task {
        id: "t".into(),
        conversation_id: "c".into(),
        goal: "capital".into(),
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
    let result = executor.run(&plan, &mut ctx).await.expect("plan runs");
    assert!(
        result.error.is_none(),
        "delegate step failed: {:?}",
        result.error
    );
    let contract = match result.completed.get(&0) {
        Some(StepOutput::Json(v)) => v.clone(),
        other => panic!("expected the typed contract, got {other:?}"),
    };
    (calls.load(Ordering::SeqCst), contract)
}

fn model() -> PathBuf {
    let dir = std::env::var_os("SOVEREIGN_MODELS_DIR")
        .map(PathBuf::from)
        .expect("SOVEREIGN_MODELS_DIR names the GGUF directory, absolute");
    let path = dir.join(MODEL);
    assert!(path.is_file(), "{} is missing", path.display());
    path
}

fn assert_round_trip(engine: &str, calls: usize, contract: &serde_json::Value) {
    eprintln!("{engine}: tool calls {calls}, contract {contract}");
    assert!(calls >= 1, "{engine}: the worker never called the tool");
    let capital = contract["capital"].as_str().unwrap_or_default();
    assert!(
        capital.contains(CAPITAL),
        "{engine}: the tool's result did not reach the contract: {contract}"
    );
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots target/debug/llama-server on Qwen3.5-0.8B; run with --ignored"]
async fn a_delegate_step_round_trips_a_tool_on_llama_server() {
    let path = model();
    // The test binary is target/debug/deps/<name>; the server sits beside deps.
    let exe = std::env::current_exe().expect("test binary path");
    let server = exe
        .parent()
        .and_then(|d| d.parent())
        .unwrap()
        .join("llama-server");
    assert!(server.is_file(), "{} is missing", server.display());
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("free port");
        l.local_addr().unwrap().port()
    };
    let _server = Killed(
        Command::new(&server)
            .args(["-m", path.to_str().unwrap(), "--port", &port.to_string()])
            .args(["-c", "8192", "-ngl", "99", "--alias", "qwen"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("llama-server spawns"),
    );
    let health = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !reqwest::get(&health)
        .await
        .is_ok_and(|r| r.status().is_success())
    {
        assert!(
            Instant::now() < deadline,
            "llama-server never became healthy"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let provider =
        RemoteApiProvider::new(&format!("http://127.0.0.1:{port}/v1"), None, "qwen", 8192);
    let (calls, contract) = delegate_once(Arc::new(provider)).await;
    assert_round_trip("llama-server", calls, &contract);
}
