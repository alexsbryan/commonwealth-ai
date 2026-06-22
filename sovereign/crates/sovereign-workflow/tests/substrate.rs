// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure substrate tests: parse + DAG topo (incl. cycle), templating (incl. the
//! glassbox missing-key → empty), and the Runner threading artifacts through a
//! transform + a mock tool — no daemon, no weights, no MCP.

use std::sync::Arc;

use async_trait::async_trait;
use sovereign_core::error::Result as CoreResult;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope as ToolScope, StepOutput, ToolContext,
    ToolDescriptor,
};

use sovereign_workflow::model::{Artifact, Scope};
use sovereign_workflow::{template, Runner, StepRegistry, Workflow};

// ── A mock tool: echoes its `text` param ──────────────────────

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "echo".to_string(),
            name: "echo".to_string(),
            description: "echo the `text` param back as text".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: ToolScope::Session,
            output_schema: None,
        }
    }
    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }
    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> CoreResult<StepOutput> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(StepOutput::Text(text))
    }
}

fn echo_registry() -> Arc<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));
    Arc::new(tools)
}

// ── parse + topo ──────────────────────────────────────────────

#[test]
fn topo_orders_a_chain_from_references() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "chain"
[[step]]
id = "c"
uses = "transform:identity"
input = "{b.output}"
[[step]]
id = "a"
uses = "transform:upper"
input = "x"
[[step]]
id = "b"
uses = "transform:identity"
input = "{a.output}"
"#,
    )
    .unwrap();
    let order = wf.topo_order().unwrap();
    let ids: Vec<&str> = order.iter().map(|&i| wf.steps[i].id.as_str()).collect();
    // a (no deps) before b before c, regardless of authored order.
    assert!(ids.iter().position(|&x| x == "a") < ids.iter().position(|&x| x == "b"));
    assert!(ids.iter().position(|&x| x == "b") < ids.iter().position(|&x| x == "c"));
}

#[test]
fn cycle_is_a_loud_error() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "loop"
[[step]]
id = "a"
uses = "transform:identity"
input = "{b.output}"
[[step]]
id = "b"
uses = "transform:identity"
input = "{a.output}"
"#,
    )
    .unwrap();
    let err = wf.topo_order().unwrap_err().to_string();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn duplicate_step_id_rejected() {
    let err = Workflow::parse(
        r#"
[workflow]
name = "dup"
[[step]]
id = "a"
uses = "transform:identity"
[[step]]
id = "a"
uses = "transform:upper"
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("duplicate"), "{err}");
}

// ── templating ────────────────────────────────────────────────

#[test]
fn resolve_substitutes_item_and_step_refs_and_warns_on_missing() {
    let mut scope = Scope::default();
    scope.item.insert("path".into(), "/notes/a.md".into());
    scope.completed.insert(
        "read".into(),
        Artifact {
            type_tag: "text".into(),
            output: StepOutput::Text("BODY".into()),
        },
    );
    let out = template::resolve_str("{item.path} :: {read.output} :: {gone.key}", &scope);
    // item field + completed step resolve; an unknown ref → empty string.
    assert_eq!(out, "/notes/a.md :: BODY :: ");
}

// ── runner end-to-end (transform + mock tool) ─────────────────

#[tokio::test]
async fn runner_threads_artifacts_per_item() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "echo-upper"

[source]
type = "inline"
items = ["alpha", "beta"]

[[step]]
id = "up"
uses = "transform:upper"
input = "{item.name}"

[[step]]
id = "echo"
uses = "tool:echo"
params = { text = "[{up.output}]" }
"#,
    )
    .unwrap();

    let registry = StepRegistry::new(None, echo_registry());
    let report = Runner::new(registry).run(&wf, 2).await.unwrap();

    assert_eq!(report.ok_count(), 2);
    assert_eq!(report.failed_count(), 0);
    let mut finals: Vec<String> = report
        .items
        .iter()
        .map(|i| i.result.as_ref().unwrap().clone())
        .collect();
    finals.sort();
    // each item flowed item.name → upper → echo([UPPER])
    assert_eq!(finals, vec!["[ALPHA]".to_string(), "[BETA]".to_string()]);
}

#[tokio::test]
async fn model_step_without_a_daemon_errors_clearly() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "needs-model"
[[step]]
id = "m"
uses = "model:thoughtful"
prompt = "hi"
"#,
    )
    .unwrap();
    // No inference provider wired → resolving the model step must error, not panic.
    let registry = StepRegistry::new(None, Arc::new(ToolRegistry::new()));
    let err = Runner::new(registry).run(&wf, 1).await.unwrap_err().to_string();
    assert!(err.contains("daemon"), "{err}");
}
