// SPDX-License-Identifier: AGPL-3.0-or-later
//! Knowledge-gym replay loop + predicate evaluation.
//!
//! [`run_once`] dispatches on the fixture's declared
//! [`ProductionPath`]. `Executor` goes to [`super::production`], which drives
//! the real `ReasonWithTools` step; `Raw` stays here in [`run_raw_once`],
//! which POSTs an OpenAI `tools[]` array at `/v1/chat/completions` — a
//! surface no product turn takes, kept for model-only measurement and
//! labelled everywhere it is reported.
//!
//! Every predicate in [`eval_block`] reads the replay's
//! [`ToolLedgerEntry`] rows and nothing else, whichever driver produced
//! them. That is the one decider for "did the tool fire" (ARCH §10.6); the
//! OpenAI `tool_calls` parse now exists only inside `run_raw_once`, where it
//! belongs.
//!
//! Phase 2 of Gym (Tool-Mastery follow-up) added multi-turn
//! replay. A `Fixture` carries `Vec<TurnSpec>` (length 1 for
//! single-turn back-compat); the raw driver walks them sequentially,
//! preserving conversation history between user turns. Each
//! per-turn replay runs the same tool-call sub-loop —
//! `MAX_TOOL_LOOPS` iterations of "POST → parse response →
//! inject mock evidence if tool_calls present".

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use super::ledger::{ProductionPath, ToolLedgerEntry, TurnLedger};
use super::production::{self, ExecutorHost};
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
    /// Force every fixture onto [`ProductionPath::Raw`] (`--raw`), whatever
    /// it declares. Model-only measurement; the report labels it.
    pub force_raw: bool,
    /// Built once per run and shared by every executor-path replay.
    pub executor_host: ExecutorHost,
}

impl RunnerCfg {
    /// Which path this fixture actually runs on, after `--raw`.
    pub fn path_for(&self, fx: &Fixture) -> ProductionPath {
        if self.force_raw {
            ProductionPath::Raw
        } else {
            fx.path
        }
    }
}

/// One user turn's outcome — the assistant's tool calls during
/// this turn, the final assistant message, and any per-turn
/// timing. Aggregated into [`Transcript`] across turns.
#[derive(Debug, Default, Serialize)]
pub struct TurnTranscript {
    /// This turn's tool ledger — the ONE record every predicate reads.
    pub tool_calls: Vec<ToolLedgerEntry>,
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
    pub fn all_tool_calls(&self) -> Vec<&ToolLedgerEntry> {
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
    /// The replay RAN and every predicate held.
    ///
    /// `runner_error` is deliberately still part of this: a replay that never
    /// ran did not pass. What changed on 2026-09-04 is that it is no longer
    /// the COMPLEMENT of this — see [`ReplayReport::errored`].
    fn passed(&self) -> bool {
        !self.errored() && self.predicates.iter().all(|p| p.passed)
    }

    /// The replay NEVER RAN — the daemon refused it (a 503 under load), the
    /// request could not be built, a turn errored out.
    ///
    /// Split from `passed` because the two are not complements and treating
    /// them as such is ARCH §18.3 exactly: `pass_count` counted an errored
    /// replay as a failure, so a fixture whose three replays all 503'd under
    /// a peer's load scored `0/3` — a number indistinguishable at the lane
    /// from three real predicate failures. A replay that never ran is
    /// could-not-judge, never failed.
    fn errored(&self) -> bool {
        self.transcript.runner_error.is_some()
    }

    /// The runner's own reason, when it never ran.
    fn error_reason(&self) -> Option<&str> {
        self.transcript.runner_error.as_deref()
    }
}

#[derive(Debug, Serialize)]
pub struct FixtureReport {
    pub slug: String,
    /// The path this fixture ACTUALLY ran on — the fixture's declaration,
    /// unless `--raw` overrode it. Reported per fixture because a lane that
    /// does not say which surface it measured cannot be read (ARCH §18.1).
    pub path: ProductionPath,
    pub replays: Vec<ReplayReport>,
}

impl FixtureReport {
    pub fn pass_count(&self) -> usize {
        self.replays.iter().filter(|r| r.passed()).count()
    }

    /// Replays that never ran. Reported beside the passes rather than folded
    /// into them, so a reader can tell a failing fixture from an unjudgeable
    /// one.
    pub fn errored_count(&self) -> usize {
        self.replays.iter().filter(|r| r.errored()).count()
    }

    /// Replays that actually reached a verdict — the DENOMINATOR any rate
    /// over this fixture is honest about.
    pub fn judged_count(&self) -> usize {
        self.replays.len() - self.errored_count()
    }

    /// The first runner error, for a reason line that names what happened.
    pub fn first_error(&self) -> Option<&str> {
        self.replays.iter().find_map(|r| r.error_reason())
    }

    /// Passes over JUDGED replays.
    ///
    /// `None` when nothing was judged: a fixture whose every replay 503'd has
    /// no rate, and returning `0.0` there is the silent substitution that
    /// made a busy host look like a broken model (ARCH §18.3).
    pub fn pass_rate(&self) -> Option<f32> {
        let judged = self.judged_count();
        if judged == 0 {
            None
        } else {
            Some(self.pass_count() as f32 / judged as f32)
        }
    }

    pub fn human_lines(&self) -> Vec<String> {
        let mut lines = vec![match self.path.caveat() {
            Some(c) => format!("path: {} [{c}]", self.path.as_str()),
            None => format!("path: {}", self.path.as_str()),
        }];
        lines.push(match self.pass_rate() {
            Some(rate) => format!(
                "replays: {}, passed: {}/{} ({:.0}%){}",
                self.replays.len(),
                self.pass_count(),
                self.judged_count(),
                rate * 100.0,
                if self.errored_count() > 0 {
                    format!("  [{} never ran]", self.errored_count())
                } else {
                    String::new()
                }
            ),
            None => format!(
                "replays: {}, COULD-NOT-JUDGE — none ran ({})",
                self.replays.len(),
                self.first_error().unwrap_or("no reason recorded"),
            ),
        });
        for (i, r) in self.replays.iter().enumerate() {
            if let Some(err) = &r.transcript.runner_error {
                lines.push(format!("  [{i}] RUNNER ERROR: {err}"));
                continue;
            }
            // What the PATH did with the evidence, before what the model did
            // with it. A path that dispatched the tool, got rows, and counted
            // none of them has lost the evidence upstream of anything the
            // citation predicates can see.
            for turn in &r.transcript.turns {
                let l = TurnLedger {
                    entries: turn.tool_calls.clone(),
                };
                if let Some(note) = production::evidence_delivery_note(&l) {
                    lines.push(format!("  [{i}] ! {note}"));
                }
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

/// One fixture's rollup, with NAMED fields.
///
/// Was a positional `(String, usize, usize, f32)` until 2026-09-04. The
/// errored count has no honest slot in a 4-tuple, and appending a fifth cell
/// is identity-from-position (ARCH §7.5) on a wire two crates read.
#[derive(Debug, Serialize)]
pub struct FixtureRollup {
    pub slug: String,
    /// The surface this fixture's replays ran through.
    pub path: ProductionPath,
    /// `"not-the-product"` on [`ProductionPath::Raw`], absent otherwise. A
    /// pass on the raw endpoint is a fact about the model's function-calling
    /// adapter, and the wire says so rather than leaving the reader to know it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caveat: Option<&'static str>,
    pub passed: usize,
    /// Replays that never ran. `passed + errored <= replays` always.
    pub errored: usize,
    pub replays: usize,
    /// Passes over JUDGED replays; `null` when nothing was judged.
    pub pass_rate: Option<f32>,
}

impl FixtureRollup {
    /// Replays that reached a verdict — the denominator `pass_rate` uses.
    pub fn judged(&self) -> usize {
        self.replays - self.errored
    }
}

#[derive(Debug, Serialize)]
pub struct AggregateSummary {
    pub fixtures: usize,
    pub total_replays: usize,
    pub total_passes: usize,
    /// Replays that never ran, across every fixture. A run with a non-zero
    /// count here has not measured what its `pass_rate` appears to say.
    pub total_errored: usize,
    /// Replays that reached a verdict and did not pass. On the wire beside
    /// the other two so a reader of the JSON gets the whole four-verdict
    /// distribution without deriving it (ARCH §18.2 as amended).
    pub total_failed: usize,
    pub pass_rate: f32,
    pub per_fixture: Vec<FixtureRollup>,
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
        // The four-verdict distribution, ALWAYS, not just the failures.
        //
        // ARCH §18.2 as amended: the two verdicts that make no claim are owed,
        // not free. A gym where most replays abstain has not been retargeted,
        // it has been silenced — and this line is the only thing that shows
        // the difference at a glance. `never-ran` is the same count as
        // could-not-judge here because a knowledge-gym replay that did not run
        // has exactly one cause (the daemon refused it); the two columns split
        // when a lane gains a second one.
        lines.push(format!(
            "verdicts: passed {}  failed {}  could-not-judge {}  (of {} replays)",
            self.total_passes, self.total_failed, self.total_errored, self.total_replays,
        ));
        for f in &self.per_fixture {
            let path = match f.caveat {
                Some(c) => format!(" [{} · {c}]", f.path.as_str()),
                None => format!(" [{}]", f.path.as_str()),
            };
            match f.pass_rate {
                Some(rate) => lines.push(format!(
                    "  {}{path}: {}/{} ({:.0}%){}",
                    f.slug,
                    f.passed,
                    f.judged(),
                    rate * 100.0,
                    if f.errored > 0 {
                        format!("  [{} never ran]", f.errored)
                    } else {
                        String::new()
                    }
                )),
                None => lines.push(format!(
                    "  {}{path}: COULD-NOT-JUDGE — all {} replay(s) never ran",
                    f.slug, f.errored
                )),
            }
        }
        lines
    }
}

pub fn summarise(reports: &[FixtureReport]) -> AggregateSummary {
    let total_replays: usize = reports.iter().map(|r| r.replays.len()).sum();
    let total_passes: usize = reports.iter().map(|r| r.pass_count()).sum();
    let total_errored: usize = reports.iter().map(|r| r.errored_count()).sum();
    // Over JUDGED replays. Dividing by `total_replays` charged every 503 to
    // the model, which is what made a contended host read as a regression.
    let judged = total_replays - total_errored;
    let pass_rate = if judged == 0 {
        0.0
    } else {
        total_passes as f32 / judged as f32
    };
    AggregateSummary {
        fixtures: reports.len(),
        total_replays,
        total_passes,
        total_errored,
        total_failed: judged.saturating_sub(total_passes),
        pass_rate,
        per_fixture: reports
            .iter()
            .map(|r| FixtureRollup {
                slug: r.slug.clone(),
                path: r.path,
                caveat: r.path.caveat(),
                passed: r.pass_count(),
                errored: r.errored_count(),
                replays: r.replays.len(),
                pass_rate: r.pass_rate(),
            })
            .collect(),
    }
}

pub async fn run_fixture_replays(
    client: &reqwest::Client,
    cfg: &RunnerCfg,
    fx: &Fixture,
) -> FixtureReport {
    let path = cfg.path_for(fx);
    let mut replays = Vec::with_capacity(cfg.replays as usize);
    for _ in 0..cfg.replays {
        let tx = run_once(client, cfg, fx, path).await;
        let predicates = evaluate_predicates(fx, &tx);
        replays.push(ReplayReport {
            transcript: tx,
            predicates,
        });
    }
    FixtureReport {
        slug: fx.slug.clone(),
        path,
        replays,
    }
}

/// One replay, on the path the fixture declared.
///
/// The dispatch is a `match` on a closed set, so a path that has no driver is
/// a compile error rather than a silent fall-through to the raw endpoint —
/// which is the whole failure this retarget exists to close.
async fn run_once(
    client: &reqwest::Client,
    cfg: &RunnerCfg,
    fx: &Fixture,
    path: ProductionPath,
) -> Transcript {
    match path {
        ProductionPath::Raw => run_raw_once(client, cfg, fx).await,
        ProductionPath::Executor => run_executor_once(cfg, fx).await,
        // `attached-doc` has no driver, and `load_fixtures` refuses a fixture
        // that declares it — loudly, at load, exit non-zero. So this is
        // unreachable, and it PANICS rather than degrading to a
        // could-not-judge replay: an abstention nobody has watched be
        // necessary is not rigor (ARCH §18.2 as amended), and a lane whose
        // fixtures all abstain reads as careful while measuring nothing.
        ProductionPath::AttachedDoc => unreachable!(
            "load_fixtures refuses production_path=attached-doc; a fixture reached \
             the runner with it, which means the loader guard was removed without \
             a driver being added"
        ),
    }
}

/// One replay through the executor's `ReasonWithTools` step.
async fn run_executor_once(cfg: &RunnerCfg, fx: &Fixture) -> Transcript {
    let started = Instant::now();
    let mut tx = Transcript::default();
    match production::run_executor_turn(&cfg.executor_host, fx).await {
        Ok((ledger, final_message)) => {
            tx.turns.push(TurnTranscript {
                tool_calls: ledger.entries,
                final_message,
                model_ms: started.elapsed().as_millis(),
            });
            tx.model_ms = started.elapsed().as_millis();
        }
        Err(e) => tx.runner_error = Some(e),
    }
    tx
}

/// One replay through `POST /v1/chat/completions` with an OpenAI `tools`
/// array. NOT a path any product turn takes — see [`ProductionPath::Raw`].
///
/// Walks the fixture's turn sequence end-to-end. For each turn, builds a
/// chat-completion request that splices the new turn's user message + tool
/// declarations onto the accumulated conversation history, then runs the
/// tool-call sub-loop. Multi-turn fixtures see their prior turns' assistant +
/// tool messages in the history.
async fn run_raw_once(client: &reqwest::Client, cfg: &RunnerCfg, fx: &Fixture) -> Transcript {
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
                // and records it on the ToolLedgerEntry so the
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

            turn_tx.tool_calls.push(ToolLedgerEntry {
                loop_idx,
                name: name.clone(),
                query,
                returned_evidence_ids: returned_ids,
                returned_evidence_kinds: returned_kinds,
                cached,
                // The raw driver hands the envelope through to the model
                // verbatim; it keeps no count of its own, and `None` says so
                // rather than claiming a zero it never measured.
                path_result_count: None,
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
    tool_calls: Vec<&'a ToolLedgerEntry>,
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

    let lookup_calls: Vec<&ToolLedgerEntry> = scope
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
        let shape = gap_shape(scope.final_message.unwrap_or_default());
        push(
            out,
            "answer_acknowledges_gap",
            shape.passed(),
            shape.detail(),
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

/// The clusters `answer_acknowledges_gap` reads, kept as data so the verdict
/// and the failure detail come from one place.
///
/// Extracted from `eval_block` on 2026-09-07 so the judge has tests. It had
/// none, and it was silently wrong: on the retargeted `05_noresults_honesty`
/// the model answered "The project's knowledge base does not contain
/// information about the retry policy for mesh peer reconnects" — a textbook
/// honest gap acknowledgement — and every cluster read false, because the
/// `negated_possession` list carried "no information" but not the negated
/// CONTAINMENT family ("does not contain", "contains no"). 0/3 on an answer
/// that was right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapShape {
    /// The model says it does not have / know / hold the thing.
    pub negated_possession: bool,
    /// The model distinguishes its snapshot from the present.
    pub temporal_scope: bool,
    /// The model points somewhere else to look.
    pub external_pointer: bool,
    /// The model hedges outright.
    pub direct_uncertainty: bool,
}

impl GapShape {
    /// Unchanged from the inline form: the original also carried a
    /// `temporal_scope && negated_possession` term, which the leading
    /// `negated_possession` already subsumes. Same truth table, one term
    /// fewer.
    pub fn passed(self) -> bool {
        self.negated_possession
            || (self.temporal_scope && (self.external_pointer || self.direct_uncertainty))
            || (self.external_pointer && self.direct_uncertainty)
    }

    pub fn detail(self) -> String {
        format!(
            "neg_poss={}, temp_scope={}, ext_ptr={}, direct_unc={}",
            self.negated_possession,
            self.temporal_scope,
            self.external_pointer,
            self.direct_uncertainty
        )
    }
}

/// SHAPE-level, never bank-derived phrases (per
/// `feedback_no_teaching_to_test`). Each list describes an English pattern for
/// "I do not have this", not a sentence any fixture expects back.
pub fn gap_shape(msg: &str) -> GapShape {
    let m = msg.to_lowercase();
    let any = |ws: &[&str]| ws.iter().any(|w| m.contains(w));
    GapShape {
        negated_possession: any(&[
            // Negated HOLDING.
            "don't have",
            "do not have",
            "doesn't have",
            "don't know",
            "do not know",
            "doesn't know",
            "cannot find",
            "can't find",
            "cannot retrieve",
            // Negated CONTAINMENT — the family the 2026-09-07 run found
            // missing. "the knowledge base does not contain information
            // about X" is the same move as "I have no information about X"
            // with the subject flipped from the assistant to the store.
            "not contain",
            "contains no",
            "contain no",
            "does not have",
            // Bare absence.
            "no information",
            "no data",
            "no records",
            "no evidence",
            "no result",
            "no idea",
            "not available",
            "not in my",
            "not in the",
        ]),
        temporal_scope: any(&[
            "real-time",
            "real time",
            "live data",
            "up-to-date",
            "current information",
            "current data",
            "today's",
            "recent",
            "latest",
        ]),
        external_pointer: any(&[
            "check ",
            "visit ",
            "look at ",
            "consult ",
            "recommend checking",
            "would need to",
            "you can find",
            "you could check",
        ]),
        direct_uncertainty: any(&["unfortunately", "sorry", "unsure", "i'm not sure"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── answer_acknowledges_gap ──────────────────────────────────
    //
    // A judge change is reported in BOTH directions (ARCH §18.6). The
    // widening below admits the negated-containment family; these say what it
    // now accepts AND what it still refuses.

    #[test]
    fn gap_accepts_the_answer_that_made_the_widening_necessary() {
        // Verbatim from the 2026-09-07 raw-path run of 05_noresults_honesty,
        // which scored 0/3 on this predicate alone.
        let msg = "The project's knowledge base does not contain information \
                   about the retry policy for mesh peer reconnects.";
        let shape = gap_shape(msg);
        assert!(shape.negated_possession, "shape={shape:?}");
        assert!(shape.passed(), "shape={shape:?}");
    }

    #[test]
    fn gap_still_accepts_the_shapes_it_accepted_before() {
        for msg in [
            "I do not have access to real-time information about that.",
            "I don't know — there is no evidence in the corpus.",
            "I cannot find anything on this in the local notes.",
        ] {
            assert!(gap_shape(msg).passed(), "should still pass: {msg}");
        }
    }

    #[test]
    fn gap_refuses_a_confident_fabrication() {
        // The direction that matters: the widening must not let an answer
        // that INVENTS the missing fact through. This is what the fixture is
        // for — an empty envelope answered with a specific policy.
        let msg = "The retry policy for mesh peer reconnects is exponential \
                   backoff starting at 500ms, capped at 30 seconds, with five \
                   attempts before the peer is dropped.";
        let shape = gap_shape(msg);
        assert!(!shape.passed(), "fabrication must not pass: {shape:?}");
    }

    #[test]
    fn gap_refuses_a_plain_answer_with_no_acknowledgement() {
        let msg = "Mesh peers reconnect automatically.";
        assert!(!gap_shape(msg).passed());
    }

    #[test]
    fn gap_refuses_temporal_scope_alone() {
        // "recent" on its own is not an acknowledgement of absence — it
        // appears in plenty of confident answers. The conjunction is
        // deliberate and this pins it.
        let msg = "Recent versions of the mesh use a fixed 5-second retry.";
        let shape = gap_shape(msg);
        assert!(shape.temporal_scope, "shape={shape:?}");
        assert!(
            !shape.passed(),
            "temporal scope alone must not pass: {shape:?}"
        );
    }

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
