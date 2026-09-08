// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replay a fixture through a PRODUCTION path.
//!
//! # What this replaces
//!
//! The gym's original driver ([`super::runner::run_raw_once`]) POSTs an
//! OpenAI-shaped request with a `tools` array to `/v1/chat/completions` and
//! reads `choices[0].message.tool_calls`. No product turn takes that route:
//! `knowledge_query.rs` passes `tools: None` on both synthesis routes, and the
//! two paths that DO offer `knowledge_lookup` render it as prose and parse an
//! inline `<tool_call>` marker back out. So the gym was measuring the daemon's
//! native function-calling adapter, which is a fact about the model, not about
//! the product.
//!
//! # What runs here
//!
//! [`ProductionPath::Executor`] drives `Executor::execute_reason_with_tools`
//! by way of the public `Executor::run` — the same entry a planner-emitted
//! `reason_with_tools` step takes. Nothing about the loop is re-implemented
//! here: production builds the prompt (`build_retrieval_reasoning_prompt`),
//! production parses the `<tool_call>` through `sovereign_core::tool_loop` —
//! the ONE tool-call protocol in the crate since 2026-09-08 — production
//! dispatches through `ToolRegistry::call_cached`, and production writes the
//! `search_log` this module reads as the ledger.
//!
//! **What the gym's subject is.** Tool use: does the loop offer the right
//! tools, does the model call one, do the tool's rows reach the model, does it
//! cite what came back. The grounding contract on the CHAT path is covered by
//! chaos-monkey (`citation_faithful`, `citation_grounded`, abstention,
//! distractors) and this must not duplicate it (ARCH §19). Nothing else covers
//! the tool loop.
//!
//! Two things are the gym's: the inference provider points at the running
//! daemon (so the model is the same one `--raw` measures), and
//! `knowledge_lookup` is bound to the fixture's canned envelope instead of a
//! live corpus. The mock is bound through the SAME
//! `tool_manifest::declared_from` seam the real tool uses — including the
//! description override `KnowledgeLookupTool::declared()` applies — and
//! returns the SAME `StepOutput::Json` shape (`KnowledgeLookupResponse`), so
//! what the path does with a tool result is production's behaviour and not the
//! gym's.

use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;

use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::{Plan, Speed, Step, StepKind, StepOutput, Task, TaskStatus};
use sovereign_core::{SkillRegistry, ToolRegistry};

use super::ledger::{ToolLedgerEntry, TurnLedger};
use super::Fixture;

/// A deliberate break, used to prove the lane can go red.
///
/// ARCH §18.1: a check nobody has watched fail is not a check. Each variant
/// names a failing input a reader can reproduce with one flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sabotage {
    /// Build the `ReasonWithTools` step with an EMPTY tool list, so the
    /// production prompt never offers `knowledge_lookup`. Every fixture whose
    /// `pass.toml` asserts `should_call_knowledge_lookup = true` must go red.
    NoToolOffered,
}

impl Sabotage {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "no-tool-offered" => Some(Self::NoToolOffered),
            _ => None,
        }
    }

    pub const ALL: [&'static str; 1] = ["no-tool-offered"];
}

/// Everything the executor path needs, built once per gym run.
///
/// Held apart from the per-replay work because building an inference provider
/// per replay would put HTTP client setup inside the measurement.
pub struct ExecutorHost {
    inference: Arc<dyn InferenceProvider>,
    store: Arc<dyn StateStore>,
    skills: Arc<SkillRegistry>,
    /// Bound on production's own think→search cycles. The planner's default
    /// (`planner/mod.rs`, `max_iterations` fallback) is 6; the gym keeps the
    /// same number so the loop it drives is the loop a plan gets.
    max_iterations: usize,
    sabotage: Option<Sabotage>,
}

impl ExecutorHost {
    /// Point the executor at the running daemon.
    ///
    /// `chat_model` is the id the fixture named (`"primary"` in every fixture
    /// on disk), passed through unchanged so the production path and `--raw`
    /// ask the same model the same question.
    pub fn connect(base_url: &str, chat_model: &str, sabotage: Option<Sabotage>) -> Self {
        let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
        let inference: Arc<dyn InferenceProvider> = Arc::new(
            sovereign_inference::remote::SplitInferenceProvider::new_with_bearer(
                &v1,
                None,
                chat_model.to_string(),
                // The executor's ReasonWithTools loop never embeds. The id is
                // required by the constructor, so it names the model the
                // daemon would resolve — and if that ever changes into a real
                // call, a wrong id fails loudly rather than embedding with
                // whatever happened to be resident.
                "embed".to_string(),
                8192,
                String::new(),
            ),
        );
        Self {
            inference,
            store: Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            skills: Arc::new(SkillRegistry::new()),
            max_iterations: 6,
            sabotage,
        }
    }
}

/// What one turn's canned envelope handed back, recorded by the mock.
///
/// The ids are read off what the GYM returned, never off what the model said
/// it saw — a guard that asserts on a field the subject supplies is not a
/// guard (ARCH §18.1).
#[derive(Default)]
struct MockRecord {
    calls: Vec<MockCall>,
}

struct MockCall {
    query: Option<String>,
    ids: Vec<String>,
    kinds: Vec<String>,
    cached: bool,
}

/// The user text this turn asks. `None` when the fixture's `input.json`
/// carries no user message — refused rather than replayed with an empty
/// prompt.
fn user_text(input: &Value) -> Option<String> {
    let msgs = input.get("messages")?.as_array()?;
    msgs.iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .map(str::to_string)
}

/// Bind the fixture's canned envelope to the REAL `knowledge_lookup`
/// manifest.
///
/// The manifest supplies id, parameter schema, permissions and description —
/// so the tool line the production prompt renders is production's line, not a
/// second copy that could drift (ARCH §10.6). Only the body is the gym's, and
/// it returns the same `StepOutput::Json` variant the real tool returns.
fn mock_knowledge_lookup(
    envelope: Value,
    record: Arc<Mutex<MockRecord>>,
) -> sovereign_core::tool_manifest::DeclaredTool {
    let mut manifest = sovereign_core::tool_manifest::require("knowledge_lookup").clone();
    // The description too, exactly as `KnowledgeLookupTool::declared()` sets
    // it. Without this line the prompt renders the row's placeholder —
    // "Supplied at construction from assets/tool_description.md." — so the
    // model was asked to choose a tool it had been told nothing about, and
    // every verdict was partly a measurement of that (ARCH §18.4: validate the
    // instrument before the result). Seen in the prompt dump 2026-09-08.
    manifest.description = sovereign_tools::knowledge_lookup::TOOL_DESCRIPTION
        .trim()
        .to_string();
    sovereign_core::tool_manifest::declared_from(manifest, move |params: Value, _ctx| {
        let envelope = envelope.clone();
        let record = Arc::clone(&record);
        async move {
            let query = params
                .get("query")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // `load_turn` refuses an envelope with no `evidence` array, so
            // this default is reached only for an envelope that declared
            // `[]` — it means "no rows", never "the key was missing".
            let rows = envelope
                .get("evidence")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let ids: Vec<String> = rows
                .iter()
                .filter_map(|e| e.get("id").and_then(|s| s.as_str().map(str::to_string)))
                .collect();
            let kinds: Vec<String> = rows
                .iter()
                .filter_map(|e| {
                    e.get("source_kind")
                        .and_then(|s| s.as_str().map(str::to_string))
                })
                .collect();
            let cached = envelope
                .get("cached")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Ok(mut r) = record.lock() {
                r.calls.push(MockCall {
                    query,
                    ids,
                    kinds,
                    cached,
                });
            }
            Ok(StepOutput::Json(envelope))
        }
    })
}

/// One replay of `fx` through the executor path.
///
/// Returns the turn's ledger and the final answer, or the reason it never
/// ran. An `Err` here is could-not-judge at the fixture level, never a
/// failing predicate (ARCH §18.3).
pub async fn run_executor_turn(
    host: &ExecutorHost,
    fx: &Fixture,
) -> Result<(TurnLedger, Option<String>), String> {
    // `load_fixtures` refuses a multi-turn fixture on this path, so the
    // single-turn assumption is established before the daemon is touched.
    // Asserting rather than returning a could-not-judge keeps the abstention
    // count honest (ARCH §18.2 as amended).
    debug_assert_eq!(
        fx.turns.len(),
        1,
        "load_fixtures refuses multi-turn fixtures on production_path=executor"
    );
    let spec = fx
        .turns
        .first()
        .ok_or_else(|| "fixture has no turns to replay".to_string())?;
    let prompt = user_text(&spec.input)
        .ok_or_else(|| "input.json carries no user message to replay".to_string())?;

    let record = Arc::new(Mutex::new(MockRecord::default()));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(mock_knowledge_lookup(
        spec.mock_evidence.clone(),
        Arc::clone(&record),
    )));

    // The sabotage: production still HAS the tool registered, the step just
    // never offers it. That is the shape of the real regression this guards
    // against — a plan that stops naming `knowledge_lookup` in its step.
    let available_tools = match host.sabotage {
        Some(Sabotage::NoToolOffered) => Vec::new(),
        None => vec!["knowledge_lookup".to_string()],
    };

    let executor = Executor::new(
        Arc::clone(&host.inference),
        Arc::new(tools),
        Arc::clone(&host.store),
        Arc::new(AutoApprovalChannel),
        Arc::clone(&host.skills),
    );

    let plan = Plan {
        id: format!("gym-{}", fx.slug),
        goal: prompt.clone(),
        steps: vec![Step {
            id: 0,
            description: format!("knowledge-gym fixture {}", fx.slug),
            kind: StepKind::ReasonWithTools {
                prompt_template: prompt.clone(),
                speed: Speed::Slow,
                available_tools,
                max_iterations: host.max_iterations,
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };
    let task = Task {
        id: format!("gym-task-{}", fx.slug),
        conversation_id: uuid::Uuid::new_v4().to_string(),
        goal: prompt,
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: 0,
        updated_at: 0,
        version: 0,
    };
    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor
        .run(&plan, &mut ctx)
        .await
        .map_err(|e| format!("executor: {e}"))?;
    if let Some(err) = result.error {
        return Err(format!("executor step failed: {err:?}"));
    }
    let Some(StepOutput::ReasonWithToolsResult {
        text, search_log, ..
    }) = result.completed.get(&0)
    else {
        return Err("executor returned no ReasonWithToolsResult for step 0".to_string());
    };

    // The ledger. `search_log` is PRODUCTION's record of what it dispatched —
    // tool id, the query the model wrote, and the count of results production
    // itself found in what it handed back. The evidence ids come from the
    // mock's own record of what it returned, zipped by call order.
    let rec = record.lock().map_err(|_| "mock record poisoned")?;
    let mut ledger = TurnLedger::default();
    // Correlate by POSITION AMONG MOCKED CALLS, not by position in the log.
    // The two diverge the moment the path dispatches a tool the mock does not
    // field, and zipping by index would attribute the mock's evidence to that
    // row — printing `search: 2 row(s) returned` for a tool that was never
    // available (observed on 01_corpus_definitional, 2026-09-07, when the
    // production prompt still hardcoded `"tool":"search"` as its worked
    // example whatever the step offered; that example is now generated from
    // `available_tools`, so the divergence should be rare rather than routine
    // — but a model is free to invent a tool name at any time, and this stays
    // for that).
    let mut mocked_seen = 0usize;
    for entry in search_log.iter() {
        let m = if entry.tool_id == "knowledge_lookup" {
            let m = rec.calls.get(mocked_seen);
            mocked_seen += 1;
            m
        } else {
            None
        };
        ledger.push(ToolLedgerEntry {
            loop_idx: entry.iteration,
            name: entry.tool_id.clone(),
            query: Some(entry.query.clone()),
            returned_evidence_ids: m.map(|m| m.ids.clone()).unwrap_or_default(),
            returned_evidence_kinds: m.map(|m| m.kinds.clone()).unwrap_or_default(),
            cached: m.map(|m| m.cached).unwrap_or(false),
            path_result_count: Some(entry.result_count),
        });
    }
    Ok((ledger, Some(text.clone())))
}

/// A one-line, machine-readable note about what the path did with the
/// evidence, for the human report.
///
/// Reported separately from the predicates because it is not a verdict about
/// the model: a call that fired, returned rows, and was counted as zero
/// results is the PATH losing the evidence, and folding that into a
/// citation-predicate failure would blame the wrong half.
pub fn evidence_delivery_note(ledger: &TurnLedger) -> Option<String> {
    let mut lost = Vec::new();
    for e in &ledger.entries {
        if let Some(counted) = e.path_result_count {
            if !e.returned_evidence_ids.is_empty() && counted == 0 {
                lost.push(format!(
                    "{}: {} row(s) returned, path counted {counted}",
                    e.name,
                    e.returned_evidence_ids.len()
                ));
            }
        }
    }
    if lost.is_empty() {
        None
    } else {
        Some(format!("EVIDENCE NOT DELIVERED — {}", lost.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sabotage_parses_only_declared_names() {
        assert_eq!(
            Sabotage::parse("no-tool-offered"),
            Some(Sabotage::NoToolOffered)
        );
        assert!(Sabotage::parse("none").is_none());
        assert!(Sabotage::parse("").is_none());
    }

    #[test]
    fn user_text_takes_the_last_user_message() {
        let v = json!({"messages": [
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "reply"},
            {"role": "user", "content": "second"}
        ]});
        assert_eq!(user_text(&v).as_deref(), Some("second"));
    }

    #[test]
    fn user_text_absent_is_none_not_empty() {
        // The failing input: a fixture with only a system message must be
        // refused, not replayed with "".
        let v = json!({"messages": [{"role": "system", "content": "sys"}]});
        assert!(user_text(&v).is_none());
    }

    #[test]
    fn delivery_note_fires_when_rows_returned_and_none_counted() {
        let mut l = TurnLedger::default();
        l.push(ToolLedgerEntry {
            loop_idx: 0,
            name: "knowledge_lookup".into(),
            query: Some("q".into()),
            returned_evidence_ids: vec!["ev-T0-0000".into(), "ev-T0-0001".into()],
            returned_evidence_kinds: vec!["corpus".into(), "corpus".into()],
            cached: false,
            path_result_count: Some(0),
        });
        let note = evidence_delivery_note(&l).expect("2 rows, 0 counted must be flagged");
        assert!(note.contains("EVIDENCE NOT DELIVERED"), "note={note}");
        assert!(note.contains("2 row(s)"), "note={note}");
    }

    #[test]
    fn delivery_note_silent_when_the_path_counted_the_rows() {
        let mut l = TurnLedger::default();
        l.push(ToolLedgerEntry {
            loop_idx: 0,
            name: "knowledge_lookup".into(),
            query: Some("q".into()),
            returned_evidence_ids: vec!["ev-T0-0000".into()],
            returned_evidence_kinds: vec!["corpus".into()],
            cached: false,
            path_result_count: Some(1),
        });
        assert!(evidence_delivery_note(&l).is_none());
    }

    #[test]
    fn delivery_note_silent_when_the_path_keeps_no_count() {
        // The raw driver hands the envelope through verbatim and counts
        // nothing; `None` must not read as "counted zero".
        let mut l = TurnLedger::default();
        l.push(ToolLedgerEntry {
            loop_idx: 0,
            name: "knowledge_lookup".into(),
            query: Some("q".into()),
            returned_evidence_ids: vec!["ev-T0-0000".into()],
            returned_evidence_kinds: vec!["corpus".into()],
            cached: false,
            path_result_count: None,
        });
        assert!(evidence_delivery_note(&l).is_none());
    }
}
