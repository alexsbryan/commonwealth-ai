// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reasoning loop's empty-result bound (phase-c-16): one rephrase after an
//! empty result, and after the second empty result searching closes. Read in
//! both directions: two misses close it, one miss followed by rows does not.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::types::*;
use sovereign_core::{SkillRegistry, ToolRegistry};

/// A model that searches, with a fresh query each turn, for as long as it is
/// offered tools — the rephrasing model the bound exists for. Offered none, it
/// answers. Each request's prompt and whether it carried tools are logged.
struct AlwaysSearch {
    seen: Arc<Mutex<Vec<(String, bool)>>>,
}

#[async_trait::async_trait]
impl sovereign_core::traits::InferenceProvider for AlwaysSearch {
    async fn complete(
        &self,
        request: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        let offered = request.tools.is_some();
        let turn = {
            let mut v = self.seen.lock().expect("log");
            v.push((request.prompt.clone(), offered));
            v.len()
        };
        let text = if offered {
            format!(
                r#"<tool_call>{{"name":"knowledge_lookup","arguments":{{"query":"retry policy {turn}"}}}}</tool_call>"#
            )
        } else {
            "The knowledge base does not hold a retry policy.".to_string()
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 5,
            prompt_tokens: 0,
            model_id: "always-search".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = sovereign_core::error::Result<String>> + Send>,
        >,
    > {
        Err(sovereign_core::error::Error::NotImplemented(
            "not supported".to_string(),
        ))
    }

    async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 8192,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// Runs one `ReasonWithTools` step whose lookup returns no rows on its first
/// `empty_calls` calls and one row after that.
async fn run(empty_calls: usize) -> (StepOutput, Vec<(String, bool)>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let manifest = sovereign_core::tool_manifest::require("knowledge_lookup").clone();
    let lookup = sovereign_core::tool_manifest::declared_from(manifest, move |_params, _ctx| {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            let evidence = if n < empty_calls {
                serde_json::json!([])
            } else {
                serde_json::json!([{
                    "id": "ev-0001",
                    "source_kind": "corpus",
                    "source_id": "notes",
                    "title": "Mesh reconnect",
                    "content": "Peers retry with a doubling backoff.",
                    "confidence": 0.9,
                }])
            };
            Ok(StepOutput::Json(
                serde_json::json!({ "query": "q", "evidence": evidence }),
            ))
        }
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(lookup));
    let store: Arc<dyn sovereign_core::traits::StateStore> =
        Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let executor = Executor::new(
        Arc::new(AlwaysSearch {
            seen: Arc::clone(&seen),
        }),
        Arc::new(tools),
        store,
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );
    let plan = Plan {
        id: "empty-result".to_string(),
        goal: "What is the mesh peer reconnect retry policy?".to_string(),
        steps: vec![Step {
            id: 0,
            description: "look it up".to_string(),
            kind: StepKind::ReasonWithTools {
                prompt_template: "What is the mesh peer reconnect retry policy?".to_string(),
                speed: Speed::Slow,
                available_tools: vec!["knowledge_lookup".to_string()],
                max_iterations: 4,
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };
    let mut ctx = TaskContext {
        task: Task {
            id: "empty-task".to_string(),
            conversation_id: "empty-conv".to_string(),
            goal: "test".to_string(),
            plan: plan.clone(),
            status: TaskStatus::Running,
            completed_steps: Vec::new(),
            created_at: 0,
            updated_at: 0,
            version: 0,
        },
        completed: std::collections::HashMap::new(),
    };
    let result = executor.run(&plan, &mut ctx).await.expect("executor runs");
    assert!(result.error.is_none(), "execution should succeed");
    let out = result.completed.get(&0).expect("step 0 output").clone();
    let log = seen.lock().expect("log").clone();
    (out, log)
}

#[tokio::test]
async fn reason_with_tools_closes_searching_after_the_second_empty_result() {
    let (out, log) = run(usize::MAX).await;
    let StepOutput::ReasonWithToolsResult {
        search_log, capped, ..
    } = out
    else {
        panic!("expected ReasonWithToolsResult");
    };
    assert_eq!(
        search_log
            .iter()
            .map(|e| e.result_count)
            .collect::<Vec<_>>(),
        vec![0, 0],
        "one rephrase after an empty result, then no third search: {search_log:?}"
    );
    assert!(!capped, "closed by the bound, not by max_iterations (4)");
    let (closing, offered) = log.last().expect("a closing turn");
    assert!(!offered, "the closing turn is offered no tools");
    assert!(
        closing.contains("searching is now closed"),
        "the closing turn says why searching stopped:\n{closing}"
    );
}

#[tokio::test]
async fn reason_with_tools_keeps_searching_after_one_empty_result() {
    let (out, _) = run(1).await;
    let StepOutput::ReasonWithToolsResult {
        search_log, capped, ..
    } = out
    else {
        panic!("expected ReasonWithToolsResult");
    };
    assert_eq!(
        search_log
            .iter()
            .map(|e| e.result_count)
            .collect::<Vec<_>>(),
        vec![0, 1, 1, 1],
        "a rephrase that finds rows must not count toward the bound: {search_log:?}"
    );
    assert!(capped, "this model only stops at max_iterations");
}
