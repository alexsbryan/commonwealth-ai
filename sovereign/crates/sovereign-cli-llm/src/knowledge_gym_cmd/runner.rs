// SPDX-License-Identifier: AGPL-3.0-or-later
//! Knowledge-gym replay loop + predicate evaluation.
//!
//! Phase 2 of Gym (Tool-Mastery follow-up) added multi-turn
//! replay. A `Fixture` now carries `Vec<TurnSpec>` (length 1 for
//! single-turn back-compat); the runner walks them sequentially,
//! preserving conversation history between user turns. Each
//! per-turn replay still runs the same tool-call sub-loop —
//! `MAX_TOOL_LOOPS` iterations of "POST → parse response →
//! inject mock evidence if tool_calls present" — but the outer
//! turn dimension is new.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use super::{Fixture, TurnSpec};

const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// Max tool-call iterations within a single user turn. Catches
/// runaway loops where the model keeps calling tools without
/// converging on a final assistant message. Distinct from the
/// outer user-turn count (which is fixture-driven by `fx.turns`).
const MAX_TOOL_LOOPS: usize = 6;

pub struct RunnerCfg {
    pub base_url: String,
    pub replays: u32,
}

/// One user turn's outcome — the assistant's tool calls during
/// this turn, the final assistant message, and any per-turn
/// timing. Aggregated into [`Transcript`] across turns.
#[derive(Debug, Default, Serialize)]
pub struct TurnTranscript {
    pub tool_calls: Vec<ToolCallRecord>,
    pub final_message: Option<String>,
    pub model_ms: u128,
}

/// Full replay transcript. `turns.len() == fx.turns.len()` on
/// success; a runner-side error short-circuits with `runner_error`
/// set and `turns` truncated at the failing turn.
#[derive(Debug, Default, Serialize)]
pub struct Transcript {
    pub turns: Vec<TurnTranscript>,
    pub runner_error: Option<String>,
    pub model_ms: u128,
}

impl Transcript {
    /// Last turn's data — predicates without an explicit
    /// `[turn_N]` scope evaluate against this for single-turn
    /// back-compat. Returns an empty default when the transcript
    /// has zero turns (which only happens on early runner_error).
    pub fn last_turn(&self) -> TurnTranscript {
        self.turns.last().cloned().unwrap_or_default()
    }

    /// Flat list of every tool call across every turn. Used by
    /// aggregate predicates that don't care about turn boundaries
    /// (e.g. `expected_first_tool` looks at turn 0; legacy
    /// `should_call_knowledge_lookup` looks at the union).
    pub fn all_tool_calls(&self) -> Vec<&ToolCallRecord> {
        self.turns
            .iter()
            .flat_map(|t| t.tool_calls.iter())
            .collect()
    }

    /// The final assistant message of the LAST turn — what the
    /// citation parser scans for `[ev-Tn-NNNN]` handles. Earlier
    /// turns' messages live in `turns[N].final_message` for
    /// turn-scoped predicates.
    pub fn final_message(&self) -> Option<&str> {
        self.turns.last().and_then(|t| t.final_message.as_deref())
    }
}

impl Clone for TurnTranscript {
    fn clone(&self) -> Self {
        Self {
            tool_calls: self.tool_calls.clone(),
            final_message: self.final_message.clone(),
            model_ms: self.model_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallRecord {
    /// Tool-loop iteration index within a single user turn (the
    /// "inner" turn). Not the outer user-turn index — that's
    /// recovered from `Transcript.turns[N].tool_calls`.
    pub loop_idx: usize,
    pub name: String,
    pub query: Option<String>,
    pub returned_evidence_ids: Vec<String>,
    /// Source-kind strings (`"corpus"`, `"memory"`, `"note"`,
    /// `"web"`) for each evidence row the tool returned, in
    /// order. Indexed alongside `returned_evidence_ids`. Tracked
    /// here so the `evidence_set_includes_kind` predicate (Phase B)
    /// can assert "this turn's evidence set included a web row"
    /// without re-parsing the mock envelope.
    #[serde(default)]
    pub returned_evidence_kinds: Vec<String>,
    /// True when the tool result envelope carried `"cached":
    /// true` (Tier 4 cache hit). Tracked here so the
    /// `expect_cache_hit` predicate (Phase B) has a structural
    /// signal to evaluate against.
    #[serde(default)]
    pub cached: bool,
}

#[derive(Debug, Serialize)]
pub struct PredicateOutcome {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ReplayReport {
    pub transcript: Transcript,
    pub predicates: Vec<PredicateOutcome>,
}

impl ReplayReport {
    fn passed(&self) -> bool {
        self.transcript.runner_error.is_none() && self.predicates.iter().all(|p| p.passed)
    }
}

#[derive(Debug, Serialize)]
pub struct FixtureReport {
    pub slug: String,
    pub replays: Vec<ReplayReport>,
}

impl FixtureReport {
    pub fn pass_count(&self) -> usize {
        self.replays.iter().filter(|r| r.passed()).count()
    }

    pub fn pass_rate(&self) -> f32 {
        if self.replays.is_empty() {
            0.0
        } else {
            self.pass_count() as f32 / self.replays.len() as f32
        }
    }

    pub fn human_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "replays: {}, passed: {} ({:.0}%)",
            self.replays.len(),
            self.pass_count(),
            self.pass_rate() * 100.0
        )];
        for (i, r) in self.replays.iter().enumerate() {
            if let Some(err) = &r.transcript.runner_error {
                lines.push(format!("  [{i}] RUNNER ERROR: {err}"));
                continue;
            }
            for pred in &r.predicates {
                if pred.passed {
                    lines.push(format!("  [{i}] ✓ {} — {}", pred.name, pred.detail));
                } else {
                    lines.push(format!("  [{i}] ✗ {} — {}", pred.name, pred.detail));
                    if let Some(msg) = r.transcript.final_message() {
                        let excerpt: String = msg.chars().take(1200).collect();
                        lines.push(format!("    final_message excerpt: {excerpt}"));
                    }
                }
            }
        }
        lines
    }
}

#[derive(Debug, Serialize)]
pub struct AggregateSummary {
    pub fixtures: usize,
    pub total_replays: usize,
    pub total_passes: usize,
    pub pass_rate: f32,
    pub per_fixture: Vec<(String, usize, usize, f32)>,
}

impl AggregateSummary {
    pub fn human_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "fixtures: {}  replays: {}  passes: {} ({:.0}%)",
            self.fixtures,
            self.total_replays,
            self.total_passes,
            self.pass_rate * 100.0
        )];
        for (slug, pass, total, rate) in &self.per_fixture {
            lines.push(format!("  {slug}: {pass}/{total} ({:.0}%)", rate * 100.0));
        }
        lines
    }
}

pub fn summarise(reports: &[FixtureReport]) -> AggregateSummary {
    let total_replays: usize = reports.iter().map(|r| r.replays.len()).sum();
    let total_passes: usize = reports.iter().map(|r| r.pass_count()).sum();
    let pass_rate = if total_replays == 0 {
        0.0
    } else {
        total_passes as f32 / total_replays as f32
    };
    AggregateSummary {
        fixtures: reports.len(),
        total_replays,
        total_passes,
        pass_rate,
        per_fixture: reports
            .iter()
            .map(|r| {
                (
                    r.slug.clone(),
                    r.pass_count(),
                    r.replays.len(),
                    r.pass_rate(),
                )
            })
            .collect(),
    }
}

pub async fn run_fixture_replays(
    client: &reqwest::Client,
    cfg: &RunnerCfg,
    fx: &Fixture,
) -> FixtureReport {
    let mut replays = Vec::with_capacity(cfg.replays as usize);
    for _ in 0..cfg.replays {
        let tx = run_once(client, cfg, fx).await;
        let predicates = evaluate_predicates(fx, &tx);
        replays.push(ReplayReport {
            transcript: tx,
            predicates,
        });
    }
    FixtureReport {
        slug: fx.slug.clone(),
        replays,
    }
}

/// Walk the fixture's turn sequence end-to-end. For each turn,
/// build a chat-completion request that splices the new turn's
/// user message + tool declarations onto the accumulated
/// conversation history, then run the same tool-call sub-loop
/// the single-turn path used. Multi-turn fixtures see their
/// prior turns' assistant + tool messages in the history;
/// single-turn fixtures behave identically to the pre-Phase-2
/// runner.
async fn run_once(client: &reqwest::Client, cfg: &RunnerCfg, fx: &Fixture) -> Transcript {
    let mut tx = Transcript::default();
    let endpoint = format!("{}/v1/chat/completions", cfg.base_url.trim_end_matches('/'));

    // Accumulated conversation messages threaded across turns.
    // For turn 0 this is empty; for turn N>0 it contains turn 0..N-1's
    // system (deduped), user, assistant, and tool messages.
    let mut conversation: Vec<Value> = Vec::new();
    // The active system message — taken from turn 0's input.json.
    // Subsequent turns' system message (if any) is IGNORED because
    // a chat conversation has one system message; switching it
    // mid-conversation would confuse the model.
    let mut system_message: Option<Value> = None;
    // Tool declarations stay the same across turns (the model has
    // the same toolkit on every turn). Taken from turn 0's
    // input.json; later turns' tools[] is ignored.
    let mut tools_decl: Option<Value> = None;

    for (turn_idx, spec) in fx.turns.iter().enumerate() {
        let request = match build_turn_request(
            spec,
            &mut conversation,
            &mut system_message,
            &mut tools_decl,
            turn_idx,
        ) {
            Ok(r) => r,
            Err(e) => {
                tx.runner_error = Some(format!("build turn {turn_idx} request: {e}"));
                return tx;
            }
        };
        let mut turn_tx = TurnTranscript::default();
        let outcome = run_turn_loop(
            client,
            &endpoint,
            request,
            spec,
            &mut turn_tx,
            &mut conversation,
        )
        .await;
        tx.model_ms += turn_tx.model_ms;
        tx.turns.push(turn_tx);
        if let Err(e) = outcome {
            tx.runner_error = Some(format!("turn {turn_idx}: {e}"));
            return tx;
        }
    }
    tx
}

/// Splice turn N's input (system + new user message + tools) onto
/// the accumulated conversation. The first turn captures the
/// system message + tools[] from its input.json; later turns reuse
/// those and contribute only their user message.
///
/// Returns the chat-completion request body ready to POST.
fn build_turn_request(
    spec: &TurnSpec,
    conversation: &mut Vec<Value>,
    system_message: &mut Option<Value>,
    tools_decl: &mut Option<Value>,
    turn_idx: usize,
) -> Result<Value, String> {
    let input_messages = spec
        .input
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "input missing messages array".to_string())?;

    if turn_idx == 0 {
        // First turn — capture system + tools from input.json's
        // shape and seed the conversation with the system message
        // + the user message(s).
        if let Some(sys) = input_messages
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        {
            *system_message = Some(sys.clone());
            conversation.push(sys.clone());
        }
        if let Some(tools) = spec.input.get("tools") {
            *tools_decl = Some(tools.clone());
        }
        // Append every non-system message from input.json (typically
        // the single user message; some fixtures may carry assistant
        // primers).
        for m in input_messages {
            if m.get("role").and_then(|r| r.as_str()) == Some("system") {
                continue;
            }
            conversation.push(m.clone());
        }
    } else {
        // Subsequent turns — splice only the new user message(s).
        // System + tools were locked at turn 0. We accept system
        // and tools in the per-turn input.json (fixture authors
        // commonly copy-paste the whole shape) but ignore them
        // so the conversation doesn't acquire a second system
        // message.
        for m in input_messages {
            match m.get("role").and_then(|r| r.as_str()) {
                Some("system") | None => continue, // dedupe + skip malformed
                _ => conversation.push(m.clone()),
            }
        }
    }

    // Build the request body. We always pass `stream: false`;
    // streaming isn't useful in the gym since we wait for the
    // full response anyway.
    let mut body = json!({
        "model": spec.input.get("model").cloned().unwrap_or_else(|| json!("primary")),
        "stream": false,
        "messages": Value::Array(conversation.clone()),
    });
    if let Some(t) = spec.input.get("temperature") {
        body["temperature"] = t.clone();
    }
    if let Some(t) = tools_decl.as_ref() {
        body["tools"] = t.clone();
    }
    Ok(body)
}

/// Run the inner tool-call loop for one user turn. Bounded by
/// [`MAX_TOOL_LOOPS`] to catch runaway tool-call patterns. On
/// success returns `Ok(())` and `turn_tx.final_message` is set;
/// on tool-loop exhaustion or HTTP error returns `Err(msg)`.
///
/// Each tool result that the runner injects also lands on
/// `conversation` so subsequent turns see the full assistant +
/// tool history.
async fn run_turn_loop(
    client: &reqwest::Client,
    endpoint: &str,
    mut request: Value,
    spec: &TurnSpec,
    turn_tx: &mut TurnTranscript,
    conversation: &mut Vec<Value>,
) -> Result<(), String> {
    for loop_idx in 0..MAX_TOOL_LOOPS {
        let started = Instant::now();
        let resp = client
            .post(endpoint)
            .json(&request)
            .timeout(HTTP_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("http error loop={loop_idx}: {e}"))?;
        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .map_err(|e| format!("read body loop={loop_idx}: {e}"))?;
        turn_tx.model_ms += started.elapsed().as_millis();
        if !status.is_success() {
            return Err(format!(
                "daemon http {} loop={loop_idx}: {}",
                status.as_u16(),
                body_text.chars().take(400).collect::<String>()
            ));
        }

        let resp_json: Value = serde_json::from_str(&body_text)
            .map_err(|e| format!("parse daemon response loop={loop_idx}: {e}"))?;
        let message = resp_json
            .pointer("/choices/0/message")
            .cloned()
            .ok_or_else(|| format!("daemon response missing choices[0].message loop={loop_idx}"))?;

        let tool_calls = message
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            turn_tx.final_message = message
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // Persist the assistant's final message onto the
            // accumulated conversation so the next user turn sees
            // it as history. Without this, multi-turn fixtures
            // would lose the prior turn's answer (and any
            // cross-turn citation predicate would fail because
            // there's nothing to reference).
            conversation.push(message.clone());
            return Ok(());
        }

        // Append the assistant message with tool_calls to BOTH
        // the in-flight request (so the next loop iteration sees
        // it) AND the accumulated conversation (so the next user
        // turn sees it). Mirror the OpenAI shape.
        let msgs = request
            .get_mut("messages")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| "request.messages missing".to_string())?;
        msgs.push(message.clone());
        conversation.push(message.clone());

        for tc in tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("call_unknown")
                .to_string();
            let func = tc.get("function").cloned().unwrap_or(json!({}));
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_str = func
                .get("arguments")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    func.get("arguments")
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                })
                .unwrap_or_default();
            let args_val: Value = serde_json::from_str(&args_str).unwrap_or(Value::Null);
            let query = args_val
                .get("query")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let (result_str, returned_ids, returned_kinds, cached) = if name == "knowledge_lookup" {
                let mut payload = spec.mock_evidence.clone();
                let evidence_arr = payload
                    .get("evidence")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let ids: Vec<String> = evidence_arr
                    .iter()
                    .filter_map(|e| e.get("id").and_then(|s| s.as_str().map(str::to_string)))
                    .collect();
                let kinds: Vec<String> = evidence_arr
                    .iter()
                    .filter_map(|e| {
                        e.get("source_kind")
                            .and_then(|s| s.as_str().map(str::to_string))
                    })
                    .collect();
                // Pass-through: a mock_evidence file can carry
                // top-level `cached: true` to simulate a Tier-4
                // cache hit. The runner forwards the flag as-is
                // and records it on the ToolCallRecord so the
                // `expect_cache_hit` predicate (Phase B) can
                // evaluate against structural truth, not just
                // a "did the model see this" assumption.
                let cached_flag = payload
                    .get("cached")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // Inject explicit allowlist + the warning the tool
                // descriptor declares, on EVERY response envelope.
                // The model sees this with the evidence and the
                // copy-from-the-allowlist instinct cuts down on
                // fabrication versus a description-only nudge.
                if let Some(map) = payload.as_object_mut() {
                    map.insert(
                        "valid_citation_ids".into(),
                        Value::Array(ids.iter().map(|id| Value::String(id.clone())).collect()),
                    );
                    let warning = if ids.is_empty() {
                        "Evidence empty: cite zero ev-* ids in your final answer.".to_string()
                    } else {
                        format!(
                            "The ONLY valid citation handles in your final answer are: {}. \
                             Any other ev-* token is fabrication.",
                            ids.join(", ")
                        )
                    };
                    map.insert("citation_contract".into(), Value::String(warning));
                }
                (
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
                    ids,
                    kinds,
                    cached_flag,
                )
            } else {
                (
                    format!("(knowledge-gym: tool {name} not mocked)"),
                    Vec::new(),
                    Vec::new(),
                    false,
                )
            };

            turn_tx.tool_calls.push(ToolCallRecord {
                loop_idx,
                name: name.clone(),
                query,
                returned_evidence_ids: returned_ids,
                returned_evidence_kinds: returned_kinds,
                cached,
            });

            let tool_msg = json!({
                "role": "tool",
                "tool_call_id": id,
                "name": name,
                "content": result_str,
            });
            msgs.push(tool_msg.clone());
            conversation.push(tool_msg);
        }
    }

    Err(format!(
        "hit MAX_TOOL_LOOPS={MAX_TOOL_LOOPS} without a final message"
    ))
}

/// One predicate-evaluation scope: a slice of tool calls, the
/// final assistant message, and a label prefix that's prepended
/// to each predicate name for disambiguation in the human report.
///
/// Top-level (unscoped) predicates use `label_prefix = ""` and
/// see EVERY tool call across EVERY turn — back-compat for
/// single-turn fixtures whose `should_call_knowledge_lookup`
/// historically meant "did the tool fire at all in this replay."
/// Scoped predicates (`[turn_N]`) see only turn N's tool calls
/// and turn N's final message.
struct PredicateScope<'a> {
    tool_calls: Vec<&'a ToolCallRecord>,
    final_message: Option<&'a str>,
    label_prefix: String,
    /// Outer user-turn index this scope evaluates against. `None`
    /// for the unscoped top-level scope (which sees every turn).
    /// `Some(N)` for `[turn_N]` scoped blocks — used by
    /// `must_reference_prior_turn_evidence` to identify "earlier"
    /// turn ids.
    turn_idx: Option<usize>,
}

fn evaluate_predicates(fx: &Fixture, tx: &Transcript) -> Vec<PredicateOutcome> {
    let mut out = Vec::new();
    let pass = &fx.predicates;

    // Cross-turn citation contract: an evidence id returned in
    // ANY turn is a valid citation handle in EVERY subsequent
    // turn. The fabrication check therefore unions returned ids
    // across all turns, regardless of which turn's scope a
    // predicate is evaluated in. Tier 1's dossier renderer +
    // Tier 2's frontdoor accumulator both reinforce this — handles
    // are addressable cross-turn, fabrications are not.
    let all_returned_ids: Vec<String> = tx
        .all_tool_calls()
        .iter()
        .flat_map(|tc| tc.returned_evidence_ids.clone())
        .collect();

    // Top-level predicates — unscoped. See every tool call and
    // the LAST turn's final message. On single-turn fixtures
    // this is identical to pre-Phase-A behavior; on multi-turn
    // fixtures it gives "did this happen anywhere?" semantics.
    let top_scope = PredicateScope {
        tool_calls: tx.all_tool_calls(),
        final_message: tx.final_message(),
        label_prefix: String::new(),
        turn_idx: None,
    };
    eval_block(pass, &top_scope, &all_returned_ids, &mut out);

    // Scoped predicates — walk `pass.as_table()` for `turn_N`
    // keys (Phase A.4). Each block evaluates against ONLY that
    // turn's tool calls and final message. Predicate names get a
    // `turn_N.` prefix in the output so the human report can
    // tell which scope a failure belongs to.
    if let Some(table) = pass.as_table() {
        for (key, value) in table {
            let Some(turn_idx) = parse_turn_key(key) else {
                continue;
            };
            let turn = tx.turns.get(turn_idx);
            let turn_scope = PredicateScope {
                tool_calls: turn
                    .map(|t| t.tool_calls.iter().collect())
                    .unwrap_or_default(),
                final_message: turn.and_then(|t| t.final_message.as_deref()),
                label_prefix: format!("turn_{turn_idx}."),
                turn_idx: Some(turn_idx),
            };
            eval_block(value, &turn_scope, &all_returned_ids, &mut out);
        }
    }

    out
}

/// Parse `turn_0`, `turn_22`, etc. as a usize turn index.
/// Returns `None` for non-matching keys so the evaluator only
/// walks legitimate `[turn_N]` blocks.
fn parse_turn_key(key: &str) -> Option<usize> {
    key.strip_prefix("turn_")
        .and_then(|n| n.parse::<usize>().ok())
}

/// Body of the per-scope evaluation — the original
/// `evaluate_predicates` body lifted into a helper so Phase A.4
/// can call it once for the top-level scope and once per
/// `[turn_N]` block.
fn eval_block(
    pass: &toml::Value,
    scope: &PredicateScope,
    all_returned_ids: &[String],
    out: &mut Vec<PredicateOutcome>,
) {
    let push = |outcomes: &mut Vec<PredicateOutcome>, name: &str, passed: bool, detail: String| {
        outcomes.push(PredicateOutcome {
            name: format!("{}{}", scope.label_prefix, name),
            passed,
            detail,
        });
    };

    let lookup_calls: Vec<&ToolCallRecord> = scope
        .tool_calls
        .iter()
        .copied()
        .filter(|tc| tc.name == "knowledge_lookup")
        .collect();
    let first_tool = scope.tool_calls.first().map(|t| t.name.as_str());

    if let Some(expected) = pass
        .get("should_call_knowledge_lookup")
        .and_then(|v| v.as_bool())
    {
        let actual = !lookup_calls.is_empty();
        push(
            out,
            "should_call_knowledge_lookup",
            actual == expected,
            format!("expected={expected}, actual={actual}"),
        );
    }

    if let Some(expected) = pass.get("expected_first_tool").and_then(|v| v.as_str()) {
        let passed = first_tool == Some(expected);
        push(
            out,
            "expected_first_tool",
            passed,
            format!(
                "expected={expected}, actual={}",
                first_tool.unwrap_or("(none)")
            ),
        );
    }

    if let Some(max) = pass.get("max_lookup_calls").and_then(|v| v.as_integer()) {
        let actual = lookup_calls.len();
        push(
            out,
            "max_lookup_calls",
            actual as i64 <= max,
            format!("max={max}, actual={actual}"),
        );
    }

    if let Some(max) = pass.get("max_query_tokens").and_then(|v| v.as_integer()) {
        // Approximate token count = whitespace-split words.
        let worst = lookup_calls
            .iter()
            .filter_map(|tc| tc.query.as_ref())
            .map(|q| q.split_whitespace().count())
            .max()
            .unwrap_or(0);
        push(
            out,
            "max_query_tokens",
            (worst as i64) <= max,
            format!("max={max}, worst={worst}"),
        );
    }

    let mut cited_ids: Vec<String> = Vec::new();
    if let Some(msg) = scope.final_message {
        cited_ids = extract_evidence_ids_from_text(msg);
    }

    // Use the cross-turn union for fabrication checks (handles
    // are addressable cross-turn). Keep the scope's per-turn
    // returned ids for diagnostics (`returned_in_scope` in the
    // failure detail).
    let returned_in_scope: Vec<String> = scope
        .tool_calls
        .iter()
        .flat_map(|tc| tc.returned_evidence_ids.clone())
        .collect();
    let returned_ids: &[String] = all_returned_ids;

    if pass
        .get("must_cite_at_least_one_evidence_id")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        push(
            out,
            "must_cite_at_least_one_evidence_id",
            !cited_ids.is_empty(),
            format!("cited={cited_ids:?}"),
        );
    }

    if pass
        .get("must_not_cite_evidence_id_outside_returned")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        let bad: Vec<&String> = cited_ids
            .iter()
            .filter(|id| !returned_ids.contains(id))
            .collect();
        push(
            out,
            "must_not_cite_evidence_id_outside_returned",
            bad.is_empty(),
            format!(
                "cited={cited_ids:?}, returned_all_turns={returned_ids:?}, returned_in_scope={returned_in_scope:?}, fabricated={bad:?}"
            ),
        );
    }

    if let Some(max) = pass
        .get("max_cited_evidence_ids")
        .and_then(|v| v.as_integer())
    {
        push(
            out,
            "max_cited_evidence_ids",
            (cited_ids.len() as i64) <= max,
            format!("max={max}, cited={}", cited_ids.len()),
        );
    }

    if pass
        .get("answer_acknowledges_gap")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        let msg_l = scope.final_message.unwrap_or_default().to_lowercase();
        // SHAPE-level — no bank-derived phrases (per
        // feedback_no_teaching_to_test). The predicate looks for
        // SHAPE of "honest gap acknowledgement":
        //   negated possession ("don't have", "no information")
        //   OR temporal-scope acknowledgement ("real-time",
        //   "live data", "current") — signals the model is
        //   distinguishing its snapshot from the present
        //   OR external-source pointer ("check X", "visit Y") —
        //   the honest "go look elsewhere" move when local
        //   evidence is empty.
        // Any one of these clusters is sufficient. Multiple
        // disjoint clusters keep the predicate from over-fitting
        // to a single phrasing convention.

        let negated_possession = [
            "don't have",
            "do not have",
            "doesn't have",
            "don't know",
            "do not know",
            "doesn't know",
            "cannot find",
            "can't find",
            "cannot retrieve",
            "no information",
            "no data",
            "no records",
            "no evidence",
            "no result",
            "no idea",
            "not available",
            "not in my",
            "not in the",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let temporal_scope = [
            "real-time",
            "real time",
            "live data",
            "up-to-date",
            "current information",
            "current data",
            "today's",
            "recent",
            "latest",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let external_pointer = [
            "check ",
            "visit ",
            "look at ",
            "consult ",
            "recommend checking",
            "would need to",
            "you can find",
            "you could check",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let direct_uncertainty = ["unfortunately", "sorry", "unsure", "i'm not sure"]
            .iter()
            .any(|w| msg_l.contains(w));

        let passed = negated_possession
            || (temporal_scope && (negated_possession || external_pointer || direct_uncertainty))
            || (external_pointer && direct_uncertainty);

        push(
            out,
            "answer_acknowledges_gap",
            passed,
            format!(
                "neg_poss={negated_possession}, temp_scope={temporal_scope}, ext_ptr={external_pointer}, direct_unc={direct_uncertainty}, msg_len={}",
                msg_l.len()
            ),
        );
    }

    // Phase B predicates (Tier 5 of tool-framework expansion).

    // `must_reference_prior_turn_evidence` — true requires the
    // scope's final message to cite at least one `[ev-Tn-NNNN]`
    // handle whose turn segment is strictly less than this scope's
    // turn_idx. Only meaningful on multi-turn fixtures with
    // turn-scoped predicates (so we know what "prior" means).
    // Falls through to a skipped predicate when turn_idx is None
    // OR is 0 (turn 0 has no prior turns).
    if pass
        .get("must_reference_prior_turn_evidence")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        let current_turn = scope.turn_idx.unwrap_or(0);
        let prior_refs: Vec<String> = cited_ids
            .iter()
            .filter(|id| {
                turn_segment_of(id)
                    .map(|t| t < current_turn)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        push(
            out,
            "must_reference_prior_turn_evidence",
            !prior_refs.is_empty(),
            format!("current_turn={current_turn}, prior_refs={prior_refs:?}, cited={cited_ids:?}"),
        );
    }

    // `expect_cache_hit` — true requires at least one tool call
    // in scope to have come back with `cached: true`. Validates
    // Tier 4's cache observably from the runner side.
    if pass.get("expect_cache_hit").and_then(|v| v.as_bool()) == Some(true) {
        let hits = scope.tool_calls.iter().filter(|tc| tc.cached).count();
        push(
            out,
            "expect_cache_hit",
            hits > 0,
            format!("hits={hits} of {} tool_calls", scope.tool_calls.len()),
        );
    }

    // `evidence_set_includes_kind` — array of source-kind strings
    // (`["web"]`, `["corpus", "web"]`, `[]` for "no kinds at all").
    // True requires the scope's union of returned evidence kinds
    // to be EXACTLY the listed set (order-insensitive). Use to
    // catch escalation having fired (`["web"]`) or not having
    // fired (`["corpus"]`).
    if let Some(expected) = pass
        .get("evidence_set_includes_kind")
        .and_then(|v| v.as_array())
    {
        let expected_set: std::collections::HashSet<String> = expected
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let actual_set: std::collections::HashSet<String> = scope
            .tool_calls
            .iter()
            .flat_map(|tc| tc.returned_evidence_kinds.iter().cloned())
            .collect();
        let passed = expected_set.iter().all(|k| actual_set.contains(k));
        push(
            out,
            "evidence_set_includes_kind",
            passed,
            format!("expected_subset={expected_set:?}, actual={actual_set:?}"),
        );
    }

    // `min_tool_calls` — lower bound on the scope's total tool
    // call count. Multi-call assembly fixtures use this to
    // require the model invoked the tool at least N times.
    if let Some(min) = pass.get("min_tool_calls").and_then(|v| v.as_integer()) {
        let actual = scope.tool_calls.len();
        push(
            out,
            "min_tool_calls",
            (actual as i64) >= min,
            format!("min={min}, actual={actual}"),
        );
    }

    // `evidence_set_excludes_kind` — sibling of
    // `evidence_set_includes_kind`. List of kinds that must NOT
    // appear in the scope's returned evidence. Use for negative
    // controls (e.g. "this scope must not include web rows" =
    // escalation did NOT fire when it shouldn't have).
    if let Some(forbidden) = pass
        .get("evidence_set_excludes_kind")
        .and_then(|v| v.as_array())
    {
        let forbidden_set: std::collections::HashSet<String> = forbidden
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let actual_set: std::collections::HashSet<String> = scope
            .tool_calls
            .iter()
            .flat_map(|tc| tc.returned_evidence_kinds.iter().cloned())
            .collect();
        let intersect: Vec<&String> = forbidden_set
            .iter()
            .filter(|k| actual_set.contains(*k))
            .collect();
        push(
            out,
            "evidence_set_excludes_kind",
            intersect.is_empty(),
            format!("forbidden={forbidden_set:?}, actual={actual_set:?}, found={intersect:?}"),
        );
    }

    // `answer_attributes_conflict` — SHAPE-level. When the
    // evidence set contains genuinely-contradicting rows (mock
    // sets up A and ¬A), the model should acknowledge the
    // disagreement rather than silently pick one. Looks for
    // contrast-shape vocabulary in the final answer AND that
    // the model cited at least two distinct evidence ids (you
    // can't attribute a conflict you didn't reference both
    // sides of).
    //
    // Per `feedback_no_teaching_to_test`: the vocabulary lists
    // describe SHAPES (English contrast / negation / disagreement
    // patterns), not bank-derived phrases. Multiple disjoint
    // clusters keep the predicate from over-fitting to one
    // phrasing convention.
    if pass
        .get("answer_attributes_conflict")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        let msg_l = scope.final_message.unwrap_or_default().to_lowercase();

        let contrast_token = [
            "however",
            "but ",
            "yet",
            "whereas",
            "while",
            "on the other hand",
            "in contrast",
            "by contrast",
            "though",
            "although",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        let disagreement_token = [
            "disagree",
            "conflict",
            "contradict",
            "inconsisten",
            "tension",
            "differ",
            "different account",
            "competing",
            "at odds",
            "diverge",
            "discrepancy",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        // Plural-source acknowledgement — the model is talking
        // about multiple evidence rows, not picking one.
        let plural_sources = [
            "two sources",
            "both sources",
            "two accounts",
            "the sources",
            "the evidence rows",
            "the two pieces",
            "one source", // "one source says X, another says Y"
            "another source",
            "the other source",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        // Cited at least two distinct ids — without this, the
        // model might use contrast vocabulary while only
        // grounding in one row, which isn't conflict attribution.
        let two_plus_cited = cited_ids.len() >= 2;

        let passed = two_plus_cited && (disagreement_token || (contrast_token && plural_sources));

        push(
            out,
            "answer_attributes_conflict",
            passed,
            format!(
                "contrast={contrast_token}, disagree={disagreement_token}, \
                 plural_src={plural_sources}, two_cited={two_plus_cited}, \
                 cited={cited_ids:?}"
            ),
        );
    }

    // `answer_acknowledges_partial_match` — SHAPE-level. When
    // the mock evidence is related-but-not-directly-answering,
    // the model should acknowledge the gap between what the
    // corpus has and what the user asked rather than confidently
    // synthesising a non-answer.
    //
    // Distinct from `answer_acknowledges_gap` (which checks for
    // "I don't know" when evidence is empty) — here the evidence
    // is NON-empty, just off-target. The vocabulary lists below
    // describe SHAPES of scope-qualification / hedging, not
    // bank-derived phrases.
    if pass
        .get("answer_acknowledges_partial_match")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        let msg_l = scope.final_message.unwrap_or_default().to_lowercase();

        // Direct-answer denial: the model explicitly says the
        // evidence doesn't answer the question directly. Includes
        // the "doesn't / does not explicitly / specifically /
        // clarify / state" family — all common English shapes
        // for "source-doesn't-address-this-specific-point".
        let direct_denial = [
            "doesn't directly",
            "does not directly",
            "doesn't specifically",
            "does not specifically",
            "doesn't explicitly",
            "does not explicitly",
            "doesn't clarify",
            "does not clarify",
            "doesn't state",
            "does not state",
            "doesn't answer",
            "does not answer",
            "without directly",
            "not directly address",
            "not specifically address",
            "no explicit",
            "no specific mention",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        // Scope-qualifier: the model frames what the evidence
        // covers vs what it doesn't. The "covers X but leaves
        // out Y" / "discusses X without Y" patterns are
        // structurally identical to "doesn't include" — they
        // partition what the source has from what it doesn't.
        let scope_qualifier = [
            "doesn't cover",
            "does not cover",
            "doesn't include",
            "does not include",
            "leaves out",
            "leaving out",
            "without addressing",
            "without specifying",
            "related to",
            "in the context of",
            "tangentially",
            "adjacent to",
            "broader topic",
            "the evidence is about",
            "the corpus has",
            "no information about the specific",
            "but leaves",
            "but does not",
            "covers the", // "covers the rate but leaves out the formulation"
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        // Hedge token — softening the answer rather than
        // asserting confidently.
        let hedge_token = [
            "while",
            "although",
            "though",
            "more general",
            "the closest",
            "what's available",
            "what i can tell",
        ]
        .iter()
        .any(|w| msg_l.contains(w));

        // At least one citation — the model is referencing the
        // evidence (otherwise "no information" would be the
        // honest answer, not partial-match acknowledgement).
        let some_citation = !cited_ids.is_empty();

        let passed = some_citation && (direct_denial || (scope_qualifier && hedge_token));

        push(
            out,
            "answer_acknowledges_partial_match",
            passed,
            format!(
                "direct_denial={direct_denial}, scope_qual={scope_qualifier}, \
                 hedge={hedge_token}, citations={cited_ids:?}"
            ),
        );
    }
}

/// Extract the turn segment from an `ev-Tn-NNNN` handle. Returns
/// `None` for legacy `ev-NNNN` handles (no turn info). Used by
/// `must_reference_prior_turn_evidence` to recognise cross-turn
/// citations.
fn turn_segment_of(handle: &str) -> Option<usize> {
    let rest = handle.strip_prefix("ev-T")?;
    let dash_pos = rest.find('-')?;
    let turn_str = &rest[..dash_pos];
    turn_str.parse::<usize>().ok()
}

/// Extract `ev-*` citation handles from the model's final answer.
/// Handles both shapes:
/// - Legacy `ev-NNNN` (pre-Tier-1 fixtures)
/// - Tier 1 `ev-Tn-NNNN` (where `n` is the turn index)
///
/// Dedups + sorts the result. Used by every citation predicate
/// (fabrication, count caps, must-cite). Centralised so future
/// shape evolutions (e.g. `ev-Tn-NNN-suffix` if we ever add
/// sub-evidence handles) update in one place.
fn extract_evidence_ids_from_text(msg: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for part in msg.split(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
        if is_evidence_handle(part) {
            ids.push(part.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Matches `ev-NNNN` (legacy: digits-only suffix, ≥ 4 digits) OR
/// `ev-Tn-NNNN` (Tier 1: T-prefix turn segment then digits,
/// ≥ 4 digits in the index segment). Conservative — rejects
/// partial matches like `ev-T`, `ev-T0-0` (truncated index),
/// `ev-` (no body). The 4-digit floor matches the runtime's
/// emission format (`format!("ev-T{turn}-{idx:04}")`) and
/// prevents the extractor from over-matching truncated mentions
/// the model emits while talking ABOUT handles in prose.
fn is_evidence_handle(s: &str) -> bool {
    const MIN_INDEX_DIGITS: usize = 4;
    if let Some(rest) = s.strip_prefix("ev-") {
        // Legacy: ev-NNNN with ≥4 digits.
        if !rest.is_empty()
            && rest.len() >= MIN_INDEX_DIGITS
            && rest.chars().all(|c| c.is_ascii_digit())
        {
            return true;
        }
        // Tier 1: ev-T<digits>-<digits with ≥4>.
        if let Some(after_t) = rest.strip_prefix('T') {
            if let Some(dash_pos) = after_t.find('-') {
                let (turn, idx) = after_t.split_at(dash_pos);
                let idx = &idx[1..]; // skip the dash
                if !turn.is_empty()
                    && idx.len() >= MIN_INDEX_DIGITS
                    && turn.chars().all(|c| c.is_ascii_digit())
                    && idx.chars().all(|c| c.is_ascii_digit())
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_handle_recognised() {
        assert!(is_evidence_handle("ev-0001"));
        assert!(is_evidence_handle("ev-9999"));
        assert!(!is_evidence_handle("ev-"));
        assert!(!is_evidence_handle("ev"));
        // 4-digit minimum: truncated mentions don't count as
        // citations (the model often paraphrases handles inside
        // prose with shortened forms).
        assert!(!is_evidence_handle("ev-0"));
        assert!(!is_evidence_handle("ev-000"));
    }

    #[test]
    fn tier1_handle_recognised() {
        assert!(is_evidence_handle("ev-T0-0001"));
        assert!(is_evidence_handle("ev-T22-0000"));
        assert!(!is_evidence_handle("ev-T"));
        assert!(!is_evidence_handle("ev-T0"));
        assert!(!is_evidence_handle("ev-T-0001"));
        // 4-digit minimum on the index segment — these are
        // truncated mentions, not real citations.
        assert!(!is_evidence_handle("ev-T0-0"));
        assert!(!is_evidence_handle("ev-T0-000"));
    }

    #[test]
    fn extractor_finds_handles_in_prose() {
        let msg = "See [ev-T0-0001] for that claim and [ev-T0-0002] for the follow-up.";
        let ids = extract_evidence_ids_from_text(msg);
        assert_eq!(ids, vec!["ev-T0-0001", "ev-T0-0002"]);
    }

    #[test]
    fn extractor_dedups() {
        let msg = "[ev-T0-0001] and again [ev-T0-0001].";
        let ids = extract_evidence_ids_from_text(msg);
        assert_eq!(ids, vec!["ev-T0-0001"]);
    }

    #[test]
    fn extractor_handles_legacy_and_tier1_together() {
        let msg = "Old: [ev-0001]. New: [ev-T2-0003].";
        let ids = extract_evidence_ids_from_text(msg);
        assert_eq!(ids, vec!["ev-0001", "ev-T2-0003"]);
    }

    #[test]
    fn turn_segment_parses_tier1_handles() {
        assert_eq!(turn_segment_of("ev-T0-0001"), Some(0));
        assert_eq!(turn_segment_of("ev-T2-0001"), Some(2));
        assert_eq!(turn_segment_of("ev-T22-0000"), Some(22));
    }

    #[test]
    fn turn_segment_returns_none_for_legacy() {
        assert_eq!(turn_segment_of("ev-0001"), None);
        assert_eq!(turn_segment_of("ev-T-0001"), None);
        assert_eq!(turn_segment_of("ev-Tx-0001"), None);
        assert_eq!(turn_segment_of("ev-T0"), None);
    }

    #[test]
    fn parse_turn_key_recognises_valid_indices() {
        assert_eq!(parse_turn_key("turn_0"), Some(0));
        assert_eq!(parse_turn_key("turn_22"), Some(22));
        assert_eq!(parse_turn_key("turn_"), None);
        assert_eq!(parse_turn_key("turn_xx"), None);
        assert_eq!(parse_turn_key("not_a_turn"), None);
    }
}
