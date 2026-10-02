// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::functional::EvidenceProbeInference;
use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::types::*;
use sovereign_core::{SkillRegistry, ToolRegistry};

/// An empty envelope is stated as an absence where the model reads the
/// result. Without it the knowledge gym's 05_noresults_honesty rephrased into
/// the same empty store on 9 of 9 replays (`max_lookup_calls` 2 > 1).
#[tokio::test]
async fn reason_with_tools_states_an_empty_result_as_absent() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let inference = std::sync::Arc::new(EvidenceProbeInference {
        seen: std::sync::Arc::clone(&seen),
    });
    let store: std::sync::Arc<dyn sovereign_core::traits::StateStore> =
        std::sync::Arc::new(sovereign_store::memory::InMemoryStateStore::new());

    let manifest = sovereign_core::tool_manifest::require("knowledge_lookup").clone();
    let empty = sovereign_core::tool_manifest::declared_from(manifest, |_params, _ctx| async {
        Ok(StepOutput::Json(serde_json::json!({
            "query": "Bergson laughter",
            "evidence": [],
            "by_kind_counts": { "corpus": 0, "memory": 0, "note": 0 },
        })))
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(empty));

    let executor = Executor::new(
        inference,
        std::sync::Arc::new(tools),
        store,
        std::sync::Arc::new(AutoApprovalChannel),
        std::sync::Arc::new(SkillRegistry::new()),
    );
    let plan = Plan {
        id: "empty-result".to_string(),
        goal: "What did Bergson say about laughter?".to_string(),
        steps: vec![Step {
            id: 0,
            description: "look up Bergson".to_string(),
            kind: StepKind::ReasonWithTools {
                prompt_template: "What did Bergson say about laughter?".to_string(),
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
    let prompts = seen.lock().expect("prompt log").clone();
    assert!(prompts.len() >= 2, "saw {} prompt(s)", prompts.len());
    let after_tool = &prompts[1];
    let result_at = after_tool
        .find("[Search results for")
        .expect("the tool result is rendered");
    assert!(
        after_tool[result_at..].contains("the source does not hold this"),
        "an empty result must be stated as absent under the result itself:\n{after_tool}"
    );
}
