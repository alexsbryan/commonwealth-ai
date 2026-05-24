//! Search-not-agent runner — parallel candidate generation with
//! test-graded monotonic improvement.
//!
//! Architecture validated 2026-05-24 against the role-loop runner:
//! median 20/20 on the 5-bug cascading evaluator problem (4.2)
//! where role-loop never scored above 3/9 across the session. The
//! Python prototype that established this lives at
//! `/tmp/search_agent.py`; this is the production Rust port.
//!
//! Premise: the model is a stochastic search process. Variance is
//! a resource. Tests are the only honest judge. No role split,
//! no sticky-retry, no defensive parsing.
//!
//! Loop shape per trial:
//!   1. Setup workdir from scaffold (already done by ctx).
//!   2. Run baseline tests → record passing count.
//!   3. For each round (up to ROUNDS):
//!      a. Generate K candidate patches in parallel at varied temps.
//!      b. Apply each to a snapshot workdir, run tests.
//!      c. Pick winner that STRICTLY improves passing count.
//!      d. If no improvement: increment stall counter, widen
//!         diversity, exit when stall ≥ MAX_STALL.
//!   4. Final witness scoring happens downstream against the
//!      committed workdir state.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::task::JoinSet;
use tracing::{info, warn};

use commonwealth_agent_tools::executor::ExecCtx;

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ChatRequestRecord, ExitReason,
    TokenCounts,
};
use crate::runners::shared::{
    apply_edit, chat_body, discover_source_file, parse_response, post_chat_completion,
    render_with_line_numbers, run_tests, snapshot_dir, EditAction, ParsedResponse, TestRunResult,
};

// ── tuning ───────────────────────────────────────────────────────

const DEFAULT_CANDIDATES_PER_ROUND: usize = 4;
const DEFAULT_ROUNDS_PER_TRIAL: usize = 6;
const DEFAULT_MAX_STALL_ROUNDS: u32 = 3;
const DEFAULT_EMIT_MAX_TOKENS: u32 = 2500;
const DEFAULT_CANDIDATE_TEST_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_TEMP_LADDER: &[f32] = &[0.2, 0.4, 0.7, 0.9];
const DEFAULT_TEMP_LADDER_WIDE: &[f32] = &[0.3, 0.6, 0.9, 1.1];

// ── language detection ───────────────────────────────────────────

fn language_for_source(source_file: &str) -> crate::problem::WitnessLanguage {
    use crate::problem::WitnessLanguage;
    if source_file.ends_with(".py") {
        WitnessLanguage::Python
    } else if source_file.ends_with(".rs") {
        WitnessLanguage::Rust
    } else if source_file.ends_with(".go") {
        WitnessLanguage::Go
    } else {
        // Default to Python; the verify command will fail
        // unambiguously if the language doesn't match.
        WitnessLanguage::Python
    }
}

// ── runner ───────────────────────────────────────────────────────

pub struct SearchRunner {
    http: reqwest::Client,
    provider_url: String,
    candidates_per_round: usize,
    rounds_per_trial: usize,
    max_stall_rounds: u32,
    emit_max_tokens: u32,
    candidate_test_timeout: Duration,
}

impl SearchRunner {
    pub fn new() -> Self {
        Self::with_provider_url("http://localhost:9741/v1".into())
    }

    pub fn with_provider_url(provider_url: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            provider_url,
            candidates_per_round: DEFAULT_CANDIDATES_PER_ROUND,
            rounds_per_trial: DEFAULT_ROUNDS_PER_TRIAL,
            max_stall_rounds: DEFAULT_MAX_STALL_ROUNDS,
            emit_max_tokens: DEFAULT_EMIT_MAX_TOKENS,
            candidate_test_timeout: DEFAULT_CANDIDATE_TEST_TIMEOUT,
        }
    }
}

impl Default for SearchRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ── candidate result ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CandidateResult {
    temp: f32,
    response: Option<ParsedResponse>,
    /// Path to the candidate's snapshot workdir; the winning
    /// candidate's contents get promoted back to the canonical
    /// workdir at end of round.
    workdir: PathBuf,
    /// `Ok(test_result)` when apply + syntax + tests all completed.
    /// `Err(reason)` on any pre-test failure (apply rejected,
    /// syntax check failed, etc.). Either way the candidate is
    /// recorded in the trace.
    outcome: Result<TestRunResult, String>,
    tokens_in: u64,
    tokens_out: u64,
    wall_ms: u64,
    raw_response: String,
}

impl CandidateResult {
    fn passing(&self) -> i32 {
        match &self.outcome {
            Ok(t) => t.parsed.passed as i32,
            Err(_) => -1,
        }
    }

    fn shape_summary(&self) -> String {
        match self.response.as_ref().map(|r| &r.action) {
            Some(EditAction::RewriteFunction { name }) => format!("rewrite {name}"),
            Some(EditAction::PatchLines { start, end }) => format!("patch {start}-{end}"),
            Some(EditAction::InsertBefore { line }) => format!("insert@{line}"),
            Some(EditAction::WriteFile) => "write_file".to_string(),
            None => "<parse-failed>".to_string(),
        }
    }
}

// ── prompt rendering ─────────────────────────────────────────────

fn system_prompt() -> Value {
    json!({
        "role": "system",
        "content": "You are a careful engineer driving a test-driven repair loop. Respond with one fenced JSON action header followed by one fenced source-code block; no commentary outside the two fenced blocks.",
    })
}

fn build_user_prompt(
    source_file: &str,
    file_listing: &str,
    problem_prompt: &str,
    failing_tests: &[String],
    test_tail: &str,
    history_summary: &str,
) -> Value {
    let failing_block = if failing_tests.is_empty() {
        "  (none — all currently passing)".to_string()
    } else {
        failing_tests
            .iter()
            .map(|t| format!("  - {t}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let content = format!(
        r#"You are fixing a program so all of its tests pass.

## Problem

{problem_prompt}

## Current file (`{source_file}`, line-numbered)

```
{file_listing}
```

## Currently failing tests

{failing_block}

## Last test output

```
{test_tail}
```

## Attempts so far (for diversity — do not repeat)

{}

## Your output

Emit ONE fenced JSON action describing your edit, then ONE fenced
source code block with the new content.

```json
{{"action": "rewrite_function", "name": "<name>"}}
```
```json
{{"action": "patch_lines", "start": <int>, "end": <int>}}
```
```json
{{"action": "insert_before", "line": <int>}}
```

Pick whichever edit shape best addresses the most-impactful failing
test. Indent the source block to match the file's existing indent at
the edit site. No commentary outside the two fenced blocks.
"#,
        if history_summary.is_empty() {
            "(none — this is the first attempt)".to_string()
        } else {
            history_summary.to_string()
        }
    );
    json!({"role": "user", "content": content})
}

// ── single candidate ─────────────────────────────────────────────

async fn try_candidate(
    runner: SearchRunnerHandle,
    candidate_workdir: PathBuf,
    source_file: String,
    verify_cmd: String,
    language: crate::problem::WitnessLanguage,
    syntax_validator: Option<commonwealth_agent_tools::syntax::DynSyntaxValidator>,
    model: String,
    messages: Vec<Value>,
    temperature: f32,
    base_workdir: PathBuf,
) -> CandidateResult {
    let started = Instant::now();
    // Step 1: snapshot the canonical workdir to this candidate's
    // private scratch dir so test runs and patch applies don't
    // interfere with sibling candidates.
    if let Err(e) = snapshot_dir(&base_workdir, &candidate_workdir) {
        return CandidateResult {
            temp: temperature,
            response: None,
            workdir: candidate_workdir,
            outcome: Err(format!("snapshot: {e}")),
            tokens_in: 0,
            tokens_out: 0,
            wall_ms: started.elapsed().as_millis() as u64,
            raw_response: String::new(),
        };
    }

    // Step 2: emit
    let body = chat_body(&model, messages, Some(temperature), runner.emit_max_tokens);
    let response_json = match post_chat_completion(&runner.http, &runner.provider_url, &body).await
    {
        Ok(v) => v,
        Err(e) => {
            return CandidateResult {
                temp: temperature,
                response: None,
                workdir: candidate_workdir,
                outcome: Err(format!("daemon: {e}")),
                tokens_in: 0,
                tokens_out: 0,
                wall_ms: started.elapsed().as_millis() as u64,
                raw_response: String::new(),
            };
        }
    };
    let content = response_json
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tokens_in = response_json
        .pointer("/usage/prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let tokens_out = response_json
        .pointer("/usage/completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Step 3: parse
    let Some(parsed) = parse_response(&content) else {
        return CandidateResult {
            temp: temperature,
            response: None,
            workdir: candidate_workdir,
            outcome: Err("parse: no action+block found".into()),
            tokens_in,
            tokens_out,
            wall_ms: started.elapsed().as_millis() as u64,
            raw_response: content,
        };
    };

    // Step 4: apply (uses the executor — pre-write syntax check
    // runs here if validator is present; rejection becomes Err).
    let mut ctx = ExecCtx::new(candidate_workdir.clone());
    if let Some(v) = syntax_validator.clone() {
        ctx = ctx.with_syntax_validator(v);
    }
    if let Err(e) = apply_edit(&ctx, &source_file, &parsed).await {
        return CandidateResult {
            temp: temperature,
            response: Some(parsed),
            workdir: candidate_workdir,
            outcome: Err(format!("apply: {}", e.render_for_agent().lines().next().unwrap_or(""))),
            tokens_in,
            tokens_out,
            wall_ms: started.elapsed().as_millis() as u64,
            raw_response: content,
        };
    }

    // Step 5: run tests
    let test_result = run_tests(
        &candidate_workdir,
        &verify_cmd,
        language,
        runner.candidate_test_timeout,
    )
    .await;

    CandidateResult {
        temp: temperature,
        response: Some(parsed),
        workdir: candidate_workdir,
        outcome: Ok(test_result),
        tokens_in,
        tokens_out,
        wall_ms: started.elapsed().as_millis() as u64,
        raw_response: content,
    }
}

// ── handle shared with candidate tasks ───────────────────────────

#[derive(Clone)]
struct SearchRunnerHandle {
    http: reqwest::Client,
    provider_url: String,
    emit_max_tokens: u32,
    candidate_test_timeout: Duration,
}

impl From<&SearchRunner> for SearchRunnerHandle {
    fn from(r: &SearchRunner) -> Self {
        Self {
            http: r.http.clone(),
            provider_url: r.provider_url.clone(),
            emit_max_tokens: r.emit_max_tokens,
            candidate_test_timeout: r.candidate_test_timeout,
        }
    }
}

// ── trait impl ───────────────────────────────────────────────────

#[async_trait]
impl AgentRunner for SearchRunner {
    fn id(&self) -> &'static str {
        "search"
    }

    fn default_model_handle(&self) -> Option<&str> {
        Some("commonwealth/primary")
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        let started = Instant::now();
        let workdir = ctx.workdir;
        let base_workdir = workdir.path().to_path_buf();
        // Candidate scratch dirs MUST live outside the canonical
        // workdir — putting them inside it makes snapshot_dir copy
        // the scratch into each candidate, recursing until OS error
        // 36 ("File name too long"). Use a sibling tempdir.
        let scratch_holder =
            tempfile::tempdir().map_err(|e| AgentRunError::Io(e))?;
        let scratch_root = scratch_holder.path().to_path_buf();

        // Discover source file before any edits.
        let source_file = match discover_source_file(workdir.path()) {
            Some(f) => f,
            None => {
                warn!(problem = %ctx.problem_id, "search: no source file discovered in workdir");
                return Ok(AgentRunArtifact {
                    workdir,
                    tokens: TokenCounts::default(),
                    wall_ms: started.elapsed().as_millis() as u64,
                    exit_reason: ExitReason::Crashed {
                        stderr_tail: "no source file present in workdir".into(),
                    },
                    tool_calls: vec![],
                    stderr_tail: String::new(),
                    final_assistant_text: String::new(),
                    raw_stdout_lines: vec![],
                    request_records: vec![],
                    role_model_map_used: None,
                });
            }
        };
        let language = language_for_source(&source_file);

        let mut tokens = TokenCounts::default();
        let mut request_records: Vec<ChatRequestRecord> = vec![];
        let mut history: Vec<String> = vec![];

        // Baseline test run.
        let baseline = run_tests(
            workdir.path(),
            &ctx.verify_cmd,
            language,
            self.candidate_test_timeout,
        )
        .await;
        info!(
            problem = %ctx.problem_id,
            passed = baseline.parsed.passed,
            failed = baseline.parsed.failed,
            total = baseline.parsed.total,
            "search: baseline tests"
        );
        let mut current_passed = baseline.parsed.passed;
        let total_tests = baseline.parsed.total.max(baseline.parsed.failed.saturating_add(baseline.parsed.passed));
        let mut current_failing = baseline.parsed.failed_names.clone();
        let mut current_tail = baseline.tail.clone();
        let mut rounds_without_improvement: u32 = 0;
        let mut total_turns: u32 = 0;

        let handle = SearchRunnerHandle::from(self);

        let mut exit_reason = ExitReason::Completed;

        for round in 0..self.rounds_per_trial {
            if current_passed >= total_tests && total_tests > 0 {
                info!(problem = %ctx.problem_id, "search: all tests passing, exit");
                break;
            }
            if tokens.output >= ctx.token_budget {
                exit_reason = ExitReason::TokensExceeded {
                    cap: ctx.token_budget,
                    observed: tokens.output,
                };
                break;
            }
            if started.elapsed().as_secs() >= ctx.wall_seconds_cap {
                exit_reason = ExitReason::Timeout {
                    cap_seconds: ctx.wall_seconds_cap,
                };
                break;
            }

            let file_listing = render_with_line_numbers(&base_workdir.join(&source_file));
            let history_summary = history
                .iter()
                .rev()
                .take(6)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let messages = vec![
                system_prompt(),
                build_user_prompt(
                    &source_file,
                    &file_listing,
                    &ctx.prompt,
                    &current_failing,
                    &current_tail,
                    &history_summary,
                ),
            ];
            let temp_ladder: &[f32] = if rounds_without_improvement >= 1 {
                DEFAULT_TEMP_LADDER_WIDE
            } else {
                DEFAULT_TEMP_LADDER
            };

            let mut join_set: JoinSet<CandidateResult> = JoinSet::new();
            for i in 0..self.candidates_per_round {
                let temp = temp_ladder[i % temp_ladder.len()];
                let candidate_workdir = scratch_root.join(format!("r{round}-c{i}"));
                let handle = handle.clone();
                let source_file = source_file.clone();
                let verify_cmd = ctx.verify_cmd.clone();
                let validator = ctx.syntax_validator.clone();
                let model = ctx.model_handle.clone();
                let messages = messages.clone();
                let base_workdir = base_workdir.clone();
                join_set.spawn(async move {
                    try_candidate(
                        handle,
                        candidate_workdir,
                        source_file,
                        verify_cmd,
                        language,
                        validator,
                        model,
                        messages,
                        temp,
                        base_workdir,
                    )
                    .await
                });
            }
            let mut candidates: Vec<CandidateResult> = Vec::with_capacity(self.candidates_per_round);
            while let Some(j) = join_set.join_next().await {
                match j {
                    Ok(c) => candidates.push(c),
                    Err(e) => warn!(error = %e, "search: candidate task join failed"),
                }
            }
            total_turns = total_turns.saturating_add(candidates.len() as u32);

            // Accumulate tokens + persist per-candidate request records.
            for c in &candidates {
                tokens.input = tokens.input.saturating_add(c.tokens_in);
                tokens.output = tokens.output.saturating_add(c.tokens_out);
                request_records.push(ChatRequestRecord {
                    turn: request_records.len() as u32,
                    role: None,
                    request: json!({
                        "search_round": round,
                        "temperature": c.temp,
                        "shape": c.shape_summary(),
                    }),
                    response: json!({
                        "content": c.raw_response,
                        "passed": c.passing(),
                        "outcome": match &c.outcome {
                            Ok(t) => format!("{}/{}", t.parsed.passed, t.parsed.passed + t.parsed.failed),
                            Err(e) => format!("err: {e}"),
                        },
                    }),
                    elapsed_ms: c.wall_ms,
                });
            }

            // Pick the strict-improvement winner.
            let winner = candidates
                .iter()
                .filter(|c| c.outcome.is_ok() && (c.passing() as u32) > current_passed)
                .max_by_key(|c| c.passing());

            let candidate_summaries = candidates
                .iter()
                .map(|c| format!("{}@T{}={}", c.shape_summary(), c.temp, c.passing()))
                .collect::<Vec<_>>()
                .join(", ");

            if let Some(w) = winner {
                info!(
                    problem = %ctx.problem_id,
                    round, previous_passed = current_passed,
                    new_passed = w.passing(), shape = %w.shape_summary(), temp = w.temp,
                    "search: round improved"
                );
                // Commit: promote winner's workdir to canonical.
                if let Err(e) = snapshot_dir(&w.workdir, &base_workdir) {
                    warn!(error = %e, "search: failed to promote winner workdir");
                }
                current_passed = w.passing() as u32;
                if let Ok(t) = &w.outcome {
                    current_failing = t.parsed.failed_names.clone();
                    current_tail = t.tail.clone();
                }
                history.push(format!(
                    "round {round}: {} (T={}) → {current_passed}/{total_tests}",
                    w.shape_summary(),
                    w.temp
                ));
                rounds_without_improvement = 0;
            } else {
                rounds_without_improvement = rounds_without_improvement.saturating_add(1);
                info!(
                    problem = %ctx.problem_id,
                    round, current_passed,
                    rounds_without_improvement,
                    candidates = %candidate_summaries,
                    "search: no improvement"
                );
                history.push(format!(
                    "round {round}: no improvement [{candidate_summaries}]"
                ));
                if rounds_without_improvement >= self.max_stall_rounds {
                    exit_reason = ExitReason::SearchStalled {
                        rounds_without_improvement,
                    };
                    break;
                }
            }
        }

        // If we exhausted the rounds budget WITHOUT a stall and
        // WITHOUT all-passing, that's SearchExhaustedRounds.
        if matches!(exit_reason, ExitReason::Completed)
            && (total_tests == 0 || current_passed < total_tests)
        {
            exit_reason = ExitReason::SearchExhaustedRounds {
                rounds: self.rounds_per_trial as u32,
            };
        }

        // Synthesize a final assistant text summarizing the
        // trajectory so the downstream judges see something
        // legible. Best-effort — judges score against this text
        // and the request_records.
        let final_assistant_text = format!(
            "Search summary: {current_passed}/{total_tests} tests passing after {} rounds.\nTrajectory:\n{}",
            history.len(),
            history.join("\n")
        );

        // scratch_holder (TempDir) auto-cleans on drop.
        drop(scratch_holder);

        Ok(AgentRunArtifact {
            workdir,
            tokens,
            wall_ms: started.elapsed().as_millis() as u64,
            exit_reason,
            tool_calls: vec![],
            stderr_tail: String::new(),
            final_assistant_text,
            raw_stdout_lines: vec![],
            request_records,
            role_model_map_used: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_runner_has_stable_id() {
        let r = SearchRunner::new();
        assert_eq!(r.id(), "search");
    }

    #[test]
    fn search_runner_default_model_is_primary() {
        let r = SearchRunner::new();
        assert_eq!(r.default_model_handle(), Some("commonwealth/primary"));
    }

    #[test]
    fn search_runner_defaults_are_validated_values() {
        // Pins the defaults validated in the Python prototype
        // (2026-05-24). A future PR that lowers candidates_per_round
        // below 4 will likely regress the variance-as-resource
        // benefit; bumping rounds without bumping max_stall might
        // mask non-converging trials behind "still trying."
        let r = SearchRunner::new();
        assert_eq!(r.candidates_per_round, 4);
        assert_eq!(r.rounds_per_trial, 6);
        assert_eq!(r.max_stall_rounds, 3);
        assert_eq!(r.emit_max_tokens, 2500);
    }

    #[test]
    fn temp_ladder_default_spans_low_to_high() {
        // Diversity is the lever: covering 0.2 → 0.9 ensures both
        // fidelity (low end) and exploration (high end) on every
        // round. If a future PR narrows this range, expect Bug 4-
        // class restructure failures to come back.
        assert_eq!(DEFAULT_TEMP_LADDER.first(), Some(&0.2));
        assert_eq!(DEFAULT_TEMP_LADDER.last(), Some(&0.9));
    }

    #[test]
    fn temp_ladder_wide_pushes_above_one() {
        // After a stall round, widening above 1.0 lets the model
        // attempt qualitatively-different strategies. Capped at 1.1
        // because higher routinely produces incoherent code per the
        // sweep (2026-05-24).
        assert!(DEFAULT_TEMP_LADDER_WIDE.iter().any(|t| *t > 1.0));
        assert!(DEFAULT_TEMP_LADDER_WIDE.iter().all(|t| *t <= 1.1));
    }
}
