// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native agent runner. Drives the daemon's
//! `/v1/chat/completions` directly with the
//! `commonwealth-agent-tools` canonical primitive set. No subprocess
//! — the agent loop, tool dispatch, and detector state machine all
//! run in-process.
//!
//! The design payoff vs pi: the model sees ONLY the canonical
//! primitives (no arbitrary `bash`), and tool execution is direct
//! Rust (the executor module of `commonwealth-agent-tools`). Per the
//! plan in `~/.claude/plans/autonomous-loop-tick-tingly-clock.md`:
//! meta-skill discipline lives in the tool contract, not the
//! prompt. Native is where that thesis is measured against pi.
//!
//! Parity with `runners/pi.rs`: same exit reasons (Completed,
//! TokensExceeded, Timeout, NoProgress, WriteThrash, ModelDone,
//! Crashed), same `ThrashTracker` via
//! `runners::shared_detectors`, same `AgentRunArtifact` shape.
//! Cross-agent reports compare honestly.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};

use commonwealth_agent_tools::adapter::{
    native as native_adapter, AgentToolAdapter, TranslateOutcome,
};
use commonwealth_agent_tools::executor::ExecCtx;
use commonwealth_agent_tools::registry::Registry;
use commonwealth_agent_tools::role::{
    profile::{default_profile_for, profile_for},
    transition::{transition_after, NextRole},
    Role, RoleDossier, RoleProfile, TransitionTrigger,
};
use commonwealth_agent_tools::{summarize_for_dossier, PrimitiveKind};

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ChatRequestRecord, ExitReason,
    TokenCounts, ToolCallRecord,
};
use crate::runners::shared_detectors::{
    CycleSignal, HandoffCycleCounter, ThrashSignal, ThrashTracker, VerifySignal,
    VerifyStuckTracker, HANDOFF_CYCLE_CAP, SAME_PATH_WRITE_THRESHOLD, VERIFY_STUCK_THRESHOLD,
};

const DEFAULT_PROVIDER_URL: &str = "http://localhost:9741/v1";
const NO_PROGRESS_TOOL_CALLS_THRESHOLD: u32 = 8;
const STDERR_TAIL_CAP_BYTES: usize = 32 * 1024;
/// Per-tenure tool-call caps. Replaces PR-2's same-primitive
/// detector with substance-level counting per §C of the PR-3 plan.
/// Each cap is a pathology ceiling — a healthy tenure stays well
/// under it. Caps are conservative so they only fire when the
/// model has genuinely failed to yield out of its role, not when
/// it's making slow but real progress.
///
/// - Planner: 3. The role only legitimately calls `agent_plan` once
///   per planning cycle; 3 allows for transient parse errors but
///   catches a Planner that loops on inspect-style behavior.
/// - Implementer: 20. The from-scratch tier requires many writes
///   (Cargo.toml, src/lib.rs, optional tests); cap allows generous
///   multi-file authoring while catching write-loop pathologies
///   the same-path thrash detector misses (e.g., write A, write B,
///   write A, write B, ...).
/// - Evaluator: 10. Build + smoke + handoff is the canonical
///   3-call shape; cap allows for re-verification cycles after
///   `agent_done` consideration.
const PLANNER_TURN_CAP: u32 = 3;
const IMPLEMENTER_TURN_CAP: u32 = 20;
const EVALUATOR_TURN_CAP: u32 = 10;

fn per_tenure_turn_cap(role: Role) -> u32 {
    match role {
        Role::Planner => PLANNER_TURN_CAP,
        Role::Implementer => IMPLEMENTER_TURN_CAP,
        Role::Evaluator => EVALUATOR_TURN_CAP,
    }
}
/// Cap on max_tokens passed to a single chat completion request.
/// Without this, the runner hands the model the entire remaining
/// token budget on the first turn — under alternation-grammar mode
/// the model can emit a content-only response that exhausts the
/// budget before any tool call lands.
///
/// 8192 accommodates write_file envelopes for files up to ~260
/// lines of Python (raised from 4096 on 2026-05-23 after the 4.2
/// smoke surfaced output-side truncation: the model's full-file
/// rewrite of a 282-line evaluator landed as 53 lines on disk
/// because the response was cut off mid-stream, breaking the
/// module and trapping the model in a rejected-rewrite loop).
/// History: 1024 was too tight (3.2-lights-out, 2026-05-22);
/// 4096 was enough for 100-line files but truncated on 260+
/// (4.2-mini-evaluator, 2026-05-23). 8192 gives headroom without
/// inviting runaway content: the llguidance grammar-stop break
/// is the real "stop on schema accept" mechanism; this cap is
/// just a defensive ceiling against runaway content when grammar
/// fails to bind.
const PER_TURN_MAX_TOKENS: u64 = 8192;
/// Handoff_to_implementer events since the last `agent_plan`.
/// When this threshold is hit, the next handoff routes back to
/// Planner instead of Implementer — giving the model a chance to
/// revise the plan with full failure dossier visible. Closes the
/// "Implementer spins on same broken approach because plan doesn't
/// change" class observed on 3.2-lights-out (model wrote 4 broken
/// drafts of the GF(2) solver, each abandoning mid-function with
/// thinking-comments, because the plan never said anything
/// different from cycle 1).
///
/// 3 is one less than HANDOFF_CYCLE_CAP (6) so the model gets at
/// least one replan attempt before the cycle limit fires. Universal
/// engineering practice — equivalent to a sprint retro when
/// implementation keeps failing against the same plan.
const REPLAN_THRESHOLD: u32 = 3;

/// Consecutive turns with `tool_calls.is_empty()` before exit.
/// Closes loop class L18 — model emits content-only responses (no
/// tool envelope) under alternation grammar's plain-text path.
/// Distinct from `no_progress` (workdir-hash-based, threshold 8):
/// empty-tool-call doesn't necessarily leave the workdir unchanged
/// (the model COULD claim it wrote a file in prose), but the agent
/// loop has no observable forward motion. 3 is aggressive enough to
/// save tokens while letting one no-tool-call turn slide as a
/// thinking pause.
const EMPTY_TOOL_CALL_STREAK_THRESHOLD: u32 = 3;

/// Operating mode for the native runner.
/// - `RoleAware` (default v1) — Planner → Implementer ↔ Evaluator
///   loop with per-role profiles and the dossier handoff packet.
/// - `Monolithic` — PR 1 behavior. One system prompt, all
///   primitives available, no role transitions. Kept as the
///   apples-to-apples regression baseline so the role layer's
///   payoff is measurable as a delta against itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeMode {
    RoleAware,
    Monolithic,
}

/// Native canonical-tool runner. Construct with `NativeRunner::new()`
/// or `with_provider_url` to point at a non-default daemon.
pub struct NativeRunner {
    provider_url: String,
    http: reqwest::Client,
    mode: NativeMode,
    /// Optional per-problem build command override. When `None`,
    /// the runner reads the bound default from the context (which
    /// the bench populates from `problem.witness.resolved_build_cmd()`).
    build_cmd: Option<String>,
    /// Optional per-problem verify command override. Same shape as
    /// build_cmd.
    verify_cmd: Option<String>,
}

impl NativeRunner {
    pub fn new() -> Self {
        Self {
            provider_url: DEFAULT_PROVIDER_URL.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            mode: NativeMode::RoleAware,
            build_cmd: None,
            verify_cmd: None,
        }
    }

    /// Construct the PR-1 baseline: single-role, all-tools-at-once.
    pub fn monolithic() -> Self {
        Self {
            mode: NativeMode::Monolithic,
            ..Self::new()
        }
    }

    pub fn with_provider_url(mut self, url: impl Into<String>) -> Self {
        self.provider_url = url.into();
        self
    }

    /// Bind per-problem build/verify commands. The bench passes the
    /// problem's resolved commands here; the runner threads them
    /// into ExecCtx so `build` / `smoke` primitives run the
    /// language-appropriate shell.
    pub fn with_problem_commands(
        mut self,
        build_cmd: impl Into<String>,
        verify_cmd: impl Into<String>,
    ) -> Self {
        self.build_cmd = Some(build_cmd.into());
        self.verify_cmd = Some(verify_cmd.into());
        self
    }

    pub fn mode(&self) -> NativeMode {
        self.mode
    }
}

impl Default for NativeRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for NativeRunner {
    fn id(&self) -> &'static str {
        match self.mode {
            NativeMode::RoleAware => "native",
            NativeMode::Monolithic => "native-monolithic",
        }
    }

    fn default_model_handle(&self) -> Option<&str> {
        Some("commonwealth/primary")
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        match self.mode {
            NativeMode::Monolithic => run_native_monolithic(self, ctx).await,
            NativeMode::RoleAware => run_native_role_aware(self, ctx).await,
        }
    }
}

// ── inner loop ───────────────────────────────────────────────────

async fn run_native_monolithic(
    runner: &NativeRunner,
    ctx: AgentRunContext,
) -> Result<AgentRunArtifact, AgentRunError> {
    let problem_id = ctx.problem_id.clone();
    let workdir = ctx.workdir;
    let model_handle = ctx.model_handle.clone();
    let token_budget = ctx.token_budget;
    let wall_cap = Duration::from_secs(ctx.wall_seconds_cap);

    let started = Instant::now();
    let mut exec_ctx = ExecCtx::new(workdir.path().to_path_buf())
        // Tight per-tool-call cap on build/smoke subprocess. Was
        // `wall_seconds_cap × 2 ≈ 1 hour`, which let a single smoke
        // call eat the entire bench wall when the model wrote an
        // infinite loop (observed 4.2 2026-05-23: pytest spinning at
        // 100% CPU for 30 min consuming the whole 1800s wall cap on
        // ONE tool call). 120s is generous for well-formed code
        // (typical smoke <5s) while still bounding pathological hangs
        // at a fraction of the overall run budget.
        .with_subprocess_wall_cap(Duration::from_secs(120))
        .with_build_cmd(
            runner
                .build_cmd
                .clone()
                .unwrap_or_else(|| ctx.build_cmd.clone()),
        )
        .with_verify_cmd(
            runner
                .verify_cmd
                .clone()
                .unwrap_or_else(|| ctx.verify_cmd.clone()),
        );
    if let Some(v) = ctx.syntax_validator.clone() {
        exec_ctx = exec_ctx.with_syntax_validator(v);
    }
    let registry = Registry::with_canonical_primitives();
    let adapter = native_adapter::Adapter;

    let mut messages: Vec<Value> = Vec::new();
    messages.push(system_message());
    messages.push(user_message(&format_initial_prompt(
        &exec_ctx.workdir,
        &ctx.prompt,
    )));

    let tools = adapter.tool_descriptors();
    let mut tool_calls_record: Vec<ToolCallRecord> = Vec::new();
    let mut raw_lines: Vec<String> = Vec::new();
    let mut tokens = TokenCounts::default();
    let mut final_assistant_text = String::new();

    let mut thrash = ThrashTracker::new();
    let mut last_workdir_hash = hash_workdir(workdir.path());
    let mut consecutive_no_progress: u32 = 0;
    let mut turn: u32 = 0;

    let exit_reason: ExitReason = loop {
        turn = turn.saturating_add(1);

        // Wall-cap check at top of loop so a long subprocess
        // doesn't push us past budget.
        if started.elapsed() >= wall_cap {
            break ExitReason::Timeout {
                cap_seconds: ctx.wall_seconds_cap,
            };
        }
        // Token-budget check.
        if tokens.output >= token_budget {
            break ExitReason::TokensExceeded {
                cap: token_budget,
                observed: tokens.output,
            };
        }

        // Send chat completion.
        let resp = match send_chat_completion(
            runner,
            &model_handle,
            &messages,
            &tools,
            token_budget.saturating_sub(tokens.output).max(64),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    problem = %problem_id,
                    error = %e,
                    "native_runner: chat completion failed"
                );
                break ExitReason::Crashed {
                    stderr_tail: format!("chat completion: {e}"),
                };
            }
        };

        raw_lines.push(serde_json::to_string(&resp).unwrap_or_default());

        let usage = resp.get("usage");
        if let Some(u) = usage {
            tokens.input = u
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(tokens.input);
            tokens.output = tokens.output.saturating_add(
                u.get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            );
        }

        let choices = resp.get("choices").and_then(|v| v.as_array());
        let msg = choices
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .cloned();
        let Some(msg) = msg else {
            tracing::warn!(
                problem = %problem_id,
                "native_runner: response had no choices.message — terminating"
            );
            break ExitReason::Crashed {
                stderr_tail: "response missing choices[0].message".into(),
            };
        };

        // Capture assistant text content (if any).
        let assistant_text = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !assistant_text.is_empty() {
            final_assistant_text = assistant_text.clone();
        }

        // Echo assistant message back into history (OpenAI shape
        // demands the assistant turn be reproduced verbatim before
        // tool-result messages).
        let mut assistant_for_history = json!({
            "role": "assistant",
        });
        if !assistant_text.is_empty() {
            assistant_for_history["content"] = json!(assistant_text);
        }
        if let Some(tc) = msg.get("tool_calls") {
            assistant_for_history["tool_calls"] = tc.clone();
        }
        messages.push(assistant_for_history);

        // Tool calls.
        let tool_calls = msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            // No tool calls this turn — pi-compatible
            // termination heuristic (model has nothing to do).
            tracing::info!(
                problem = %problem_id,
                turn,
                "native_runner: assistant turn had no tool calls — terminating"
            );
            break ExitReason::Completed;
        }

        let mut model_done = false;
        for tc in &tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
            let name = fn_obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_raw = fn_obj
                .get("arguments")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    fn_obj
                        .get("arguments")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_string())
                });
            let args_value: Value = serde_json::from_str(&args_raw).unwrap_or(Value::Null);
            let args_preview = args_raw.chars().take(256).collect::<String>();

            let outcome = adapter.translate(&name, &args_value);
            let canonical_kind = outcome.canonical_kind();
            tool_calls_record.push(ToolCallRecord {
                turn,
                tool: name.clone(),
                args_preview,
                ok: matches!(outcome, TranslateOutcome::Canonical { .. }),
                canonical_kind,
            });

            // Detector update + result formatting.
            let (tool_result_content, kill_now) = match outcome {
                TranslateOutcome::Canonical {
                    canonical,
                    canonical_kind,
                } => {
                    use commonwealth_agent_tools::PrimitiveKind;
                    // Update thrash tracker BEFORE executing so a
                    // 3rd same-path write triggers without us
                    // actually running the wasteful write.
                    let signal = match canonical_kind {
                        PrimitiveKind::WriteFile => {
                            let path = match &canonical {
                                commonwealth_agent_tools::Primitive::WriteFile(args) => {
                                    Some(args.path.as_str())
                                }
                                _ => None,
                            };
                            thrash.observe_write(path)
                        }
                        PrimitiveKind::Build | PrimitiveKind::Smoke => {
                            thrash.observe_verify();
                            ThrashSignal::Continue
                        }
                        _ => ThrashSignal::Continue,
                    };
                    if let ThrashSignal::Kill { same_path_writes } = signal {
                        tracing::warn!(
                            problem = %problem_id,
                            same_path_writes,
                            threshold = SAME_PATH_WRITE_THRESHOLD,
                            path = ?thrash.last_write_path(),
                            "native_runner: write_thrash kill"
                        );
                        // Append a synthetic tool result so the
                        // OpenAI message history stays valid even
                        // if we never act on the call.
                        let content =
                            "(harness aborted: same-path write thrash detected)".to_string();
                        messages.push(tool_result_message(&id, &content));
                        break;
                    }
                    // Special-case agent_done: don't dispatch;
                    // exit immediately.
                    if matches!(canonical_kind, PrimitiveKind::AgentDone) {
                        tracing::info!(
                            problem = %problem_id,
                            "native_runner: model emitted `agent_done` — terminating run"
                        );
                        let content = json!({"done": true}).to_string();
                        messages.push(tool_result_message(&id, &content));
                        model_done = true;
                        (content, true)
                    } else {
                        match registry.dispatch(&exec_ctx, &canonical).await {
                            Ok(result) => {
                                let body =
                                    serde_json::to_string(&result.payload).unwrap_or_default();
                                (body, false)
                            }
                            Err(e) => {
                                let body = json!({"error": e.to_string()}).to_string();
                                (body, false)
                            }
                        }
                    }
                }
                TranslateOutcome::Unrecognized {
                    tool_name,
                    args_summary,
                    reason,
                } => {
                    let body = json!({
                        "error": "unrecognized tool call",
                        "tool": tool_name,
                        "args_summary": args_summary,
                        "reason": reason,
                    })
                    .to_string();
                    (body, false)
                }
                TranslateOutcome::Unknown { tool_name } => {
                    let body = json!({
                        "error": "unknown tool",
                        "tool": tool_name,
                    })
                    .to_string();
                    (body, false)
                }
            };

            messages.push(tool_result_message(&id, &tool_result_content));

            if kill_now {
                break;
            }

            // Re-check thrash kill outside the message branch.
            if matches!(thrash, _) && thrash.same_path_writes() >= SAME_PATH_WRITE_THRESHOLD {
                break;
            }
        }

        // No-progress detector: workdir-hash unchanged across
        // turns. Mirrors pi.rs exactly.
        let new_hash = hash_workdir(workdir.path());
        if new_hash == last_workdir_hash {
            consecutive_no_progress = consecutive_no_progress.saturating_add(1);
        } else {
            consecutive_no_progress = 0;
            last_workdir_hash = new_hash;
        }

        if model_done {
            break ExitReason::Completed;
        }
        if thrash.same_path_writes() >= SAME_PATH_WRITE_THRESHOLD {
            break ExitReason::WriteThrash {
                consecutive_writes: thrash.same_path_writes(),
                threshold: SAME_PATH_WRITE_THRESHOLD,
            };
        }
        if consecutive_no_progress >= NO_PROGRESS_TOOL_CALLS_THRESHOLD {
            break ExitReason::NoProgress {
                consecutive_tool_calls: consecutive_no_progress,
                threshold: NO_PROGRESS_TOOL_CALLS_THRESHOLD,
            };
        }
    };

    let wall_ms = started.elapsed().as_millis() as u64;
    Ok(AgentRunArtifact {
        workdir,
        tokens,
        wall_ms,
        exit_reason,
        tool_calls: tool_calls_record,
        stderr_tail: String::new(),
        final_assistant_text,
        raw_stdout_lines: cap_raw_lines(raw_lines),
        // Mono path doesn't capture yet — TODO if we need replay on
        // monolithic baselines. Role-aware is where debate-settling
        // happens.
        request_records: Vec::new(),
        // Monolithic has no role concept; role_model_map is not
        // consulted on this path.
        role_model_map_used: None,
    })
}

// ── role-aware loop ──────────────────────────────────────────────

async fn run_native_role_aware(
    runner: &NativeRunner,
    ctx: AgentRunContext,
) -> Result<AgentRunArtifact, AgentRunError> {
    let problem_id = ctx.problem_id.clone();
    // Read scale-derived values BEFORE moving the workdir out of ctx.
    let anchor_budget = ctx.anchor_budget_lines();
    let listing_depth = ctx.workdir_listing_depth();
    let scale = ctx.workdir_scale;
    let workdir = ctx.workdir;
    let model_handle = ctx.model_handle.clone();
    let role_model_map = ctx.role_model_map.clone();
    let token_budget = ctx.token_budget;
    let wall_cap = Duration::from_secs(ctx.wall_seconds_cap);

    // Glassbox: log the per-role dispatch map at run start so the
    // operator can verify which slot each role landed on without
    // having to read the artifact later.
    if !role_model_map.is_empty() {
        tracing::info!(
            problem = %problem_id,
            planner = role_model_map.get(Role::Planner).unwrap_or("(fallback)"),
            implementer = role_model_map.get(Role::Implementer).unwrap_or("(fallback)"),
            evaluator = role_model_map.get(Role::Evaluator).unwrap_or("(fallback)"),
            fallback = %model_handle,
            "native_runner: role→model dispatch map"
        );
    }

    let started = Instant::now();
    let mut exec_ctx = ExecCtx::new(workdir.path().to_path_buf())
        // Tight per-tool-call cap on build/smoke subprocess. Was
        // `wall_seconds_cap × 2 ≈ 1 hour`, which let a single smoke
        // call eat the entire bench wall when the model wrote an
        // infinite loop (observed 4.2 2026-05-23: pytest spinning at
        // 100% CPU for 30 min consuming the whole 1800s wall cap on
        // ONE tool call). 120s is generous for well-formed code
        // (typical smoke <5s) while still bounding pathological hangs
        // at a fraction of the overall run budget.
        .with_subprocess_wall_cap(Duration::from_secs(120))
        .with_build_cmd(
            runner
                .build_cmd
                .clone()
                .unwrap_or_else(|| ctx.build_cmd.clone()),
        )
        .with_verify_cmd(
            runner
                .verify_cmd
                .clone()
                .unwrap_or_else(|| ctx.verify_cmd.clone()),
        );
    // Wire the language-appropriate pre-build syntax validator into
    // ExecCtx so `exec_build` can short-circuit broken-syntax cases
    // with cargo-shape feedback in <50ms instead of spinning up the
    // full cargo subprocess. The monolithic path already did this;
    // the role-aware path was silently skipping it — Implementer's
    // broken syntax landed full cargo runs every time.
    if let Some(v) = ctx.syntax_validator.clone() {
        exec_ctx = exec_ctx.with_syntax_validator(v);
    }
    let registry = Registry::with_canonical_primitives();
    let adapter = native_adapter::Adapter;

    let mut tool_calls_record: Vec<ToolCallRecord> = Vec::new();
    let mut raw_lines: Vec<String> = Vec::new();
    let mut request_records: Vec<ChatRequestRecord> = Vec::new();
    let mut tokens = TokenCounts::default();
    let mut final_assistant_text = String::new();

    let mut thrash = ThrashTracker::new();
    let mut verify_stuck = VerifyStuckTracker::new();
    let mut handoff_cycles = HandoffCycleCounter::new();
    let mut last_workdir_hash = hash_workdir(workdir.path());
    let mut consecutive_no_progress: u32 = 0;
    let mut empty_tool_call_streak: u32 = 0;
    let mut cycles_since_last_plan: u32 = 0;
    let mut turn: u32 = 0;
    let mut total_role_calls: u32 = 0;

    let mut active_role = Role::initial();
    let mut role_dossier = RoleDossier::new();
    // Forced-first-tool fires only once per role-entry. Each
    // transition resets this so re-entering Evaluator forces
    // `build` again.
    let mut force_first_tool_pending = profile_for(active_role, scale).forced_first_tool;
    // Recovery-escalation flag: armed when the Implementer re-emits a
    // rejected SPLICE edit at the same site (one rejection before the
    // sticky-kill); forces its next turn onto write_file (full rewrite).
    // Consumed when the restricted subset is applied.
    let mut force_full_rewrite_pending = false;
    // Per-tenure tool-call counter. Replaces PR-2's same-primitive
    // detector with substance-level counting per §C of the PR-3
    // plan: the methodology fix is to detect stuck-state via what
    // the agent does (workdir hash + handoff cycles + per-tenure
    // budget) instead of via the *shape* of tool names. A model
    // that double-checks legitimately (re-build to see fresh
    // diagnostics) is no longer punished at turn 3 just because
    // the tool name repeats. Reset on every role flip.
    let mut tool_calls_in_tenure: u32 = 0;
    // Per-edit rollback state. Closes class: "model fixes bug N,
    // breaks previously-passing tests, has no way to recover except
    // by overwriting again." With rollback: each edit is
    // provisional until the next smoke confirms no regression.
    // Regressions revert the file and seed the dossier with an
    // explicit "your edit broke X — try a different approach" so
    // the model can vary rather than retry the same path.
    //
    // `pre_edit_snapshot` holds the workdir contents BEFORE the
    // most recent edit. `last_failed_set` holds the failing test
    // names from the most recent smoke. Regression = a test name
    // appears in `new_failed - last_failed`.
    let mut pre_edit_snapshot: Option<HashMap<PathBuf, String>> = None;
    let mut last_failed_set: Option<HashSet<String>> = None;
    // Sticky-retry detector: WINDOWED count of recent rejection
    // signatures. Fires when ≥ THRESHOLD of the last WINDOW
    // rejections share a signature.
    //
    // History:
    //   v-rollback (2026-05-23): hashed input args; missed v-darwin
    //     because the model varied edit content slightly across
    //     attempts (6 different hashes for 6 identical syntax errors).
    //   v-sigsticky (2026-05-24 AM): switched to rejection SIGNATURE
    //     and consecutive-counter; missed v-sigsticky because 7
    //     smoke-timeout rejections were interleaved with successful
    //     build calls that reset the counter.
    //   v-windowed (2026-05-24 PM, current): track the last 5
    //     rejection signatures regardless of whether successful
    //     dispatches happened between them. Fire when 3 of those 5
    //     match. Catches both prior patterns:
    //       - consecutive identical rejections (v-darwin)
    //       - rejections interleaved with successes (v-sigsticky)
    //
    // A successful dispatch does NOT reset the window — a model that
    // alternates `build → smoke (timeout) → build → smoke (timeout)`
    // is just as stuck as one that emits 3 rejections in a row.
    // Successful events naturally age out of the 5-deep window as
    // new rejections push them off.
    const STICKY_RETRY_WINDOW: usize = 5;
    const STICKY_RETRY_THRESHOLD: usize = 3;
    let mut recent_rejection_signatures: std::collections::VecDeque<String> =
        std::collections::VecDeque::with_capacity(STICKY_RETRY_WINDOW);
    // NOTE: user message is REBUILT each turn (inside the loop) so the
    // `## Workdir state` preamble reflects what files exist NOW. Was
    // built once here outside the loop pre-2026-05-22 — caused the
    // from-scratch attention bias bug where Implementer's repeat
    // tenures saw "(empty workdir) — create Cargo.toml and src/lib.rs"
    // even after Cargo.toml was written, and obediently rewrote
    // Cargo.toml instead of progressing to src/lib.rs. Replay isolation
    // proved corrected workdir state → model writes src/lib.rs on T5.
    // The workdir-state preamble is the highest-authority signal in the
    // user message; keeping it stale poisons every subsequent turn.
    // Per-tenure chat history. Closes the "role-aware Evaluator can't
    // react to its own build failure because every turn starts fresh"
    // class: assistant + tool_result messages accumulate within a
    // role's tenure so the model sees its own prior call (won't loop
    // on `build`) and the full tool_result payload (full stdout_tail,
    // not the 200-char dossier summary). Cleared on every role flip
    // — the next role starts fresh with only system + initial user +
    // rendered dossier.
    let mut role_chat_history: Vec<Value> = Vec::new();

    let exit_reason: ExitReason = 'outer: loop {
        turn = turn.saturating_add(1);
        total_role_calls = total_role_calls.saturating_add(1);
        if started.elapsed() >= wall_cap {
            break 'outer ExitReason::Timeout {
                cap_seconds: ctx.wall_seconds_cap,
            };
        }
        if tokens.output >= token_budget {
            break 'outer ExitReason::TokensExceeded {
                cap: token_budget,
                observed: tokens.output,
            };
        }

        // Build the request for the active role. The user message is
        // re-rendered every turn with the CURRENT workdir state so
        // that Implementer's "what files exist" signal matches reality
        // after each successful write (see note above the loop).
        let profile = profile_for(active_role, scale);
        // Promote any source-file declarations whose names appear in
        // the Planner's pseudocode to full-body inline anchor
        // (instead of outline). Lets the Implementer see exact
        // function contents before patching, preventing the "model
        // patches wider range than its pseudocode specified,
        // truncating siblings" failure (4.2 v-pseudo).
        let promoted_names = role_dossier
            .pseudocode
            .as_deref()
            .map(extract_referenced_names)
            .unwrap_or_default();
        let user_msg = format_initial_prompt_with_promoted(
            workdir.path(),
            &ctx.prompt,
            &promoted_names,
            anchor_budget,
            listing_depth,
        );
        let role_messages = build_role_messages(
            active_role,
            &profile,
            &role_dossier,
            &user_msg,
            &role_chat_history,
        );
        // §B grammar-termination gate: when the active role is the
        // Evaluator AND the dossier reports a passing smoke with no
        // intervening writes, swap the tool subset to
        // `[agent_done, handoff_to_implementer]`. Build/Smoke are
        // structurally unreachable for this request — the OpenAI
        // schema validator rejects any attempt to re-emit them.
        // Closes the build-loop-after-pass class observed on every
        // 2.1 native role-aware trial (HANDOFF.md 2026-05-21 night).
        let evaluator_terminating =
            matches!(active_role, Role::Evaluator) && role_dossier.smoke_just_passed();
        // Symmetric to the smoke-pass gate: when the Evaluator's last
        // build/smoke FAILED on an unchanged workdir, re-running it is
        // deterministic waste. Restrict the tool subset to
        // `[handoff_to_implementer]` so the (fast) Evaluator is forced to
        // route the failure back to the Implementer instead of
        // dead-looping into a sticky/no-progress kill (5.1-minilang
        // bombs, 2026-06-03).
        let evaluator_must_handoff =
            matches!(active_role, Role::Evaluator) && role_dossier.verification_just_failed();
        let role_tools = if evaluator_terminating {
            tracing::info!(
                problem = %problem_id,
                role = active_role.id(),
                "native_runner: smoke ok → restricting Evaluator tools to [agent_done, handoff_to_implementer] for this turn"
            );
            filter_descriptors_for(
                &adapter,
                commonwealth_agent_tools::role::EVALUATOR_TERMINATING_SUBSET,
            )
        } else if evaluator_must_handoff {
            tracing::info!(
                problem = %problem_id,
                role = active_role.id(),
                "native_runner: build/smoke failed on unchanged workdir → restricting Evaluator to [handoff_to_implementer] to force a fix"
            );
            filter_descriptors_for(
                &adapter,
                commonwealth_agent_tools::role::EVALUATOR_MUST_HANDOFF_SUBSET,
            )
        } else if matches!(active_role, Role::Implementer) && force_full_rewrite_pending {
            force_full_rewrite_pending = false;
            tracing::info!(
                problem = %problem_id,
                role = active_role.id(),
                "native_runner: recovery-escalation — Implementer restricted to [write_file] for a full rewrite after repeated splice rejection"
            );
            filter_descriptors_for(
                &adapter,
                commonwealth_agent_tools::role::IMPLEMENTER_REWRITE_SUBSET,
            )
        } else {
            filter_descriptors(&adapter, &profile)
        };
        let force_tool = force_first_tool_pending.take();

        let remaining_budget = token_budget.saturating_sub(tokens.output).max(64);
        let per_turn_max = remaining_budget.min(PER_TURN_MAX_TOKENS);
        // Resolve the model for THIS role: per-role override if set
        // (heterogeneous dispatch), else the run-wide fallback. The
        // resolution is intentionally per-request so an Evaluator
        // tenure that mid-flight transitions back to Implementer
        // picks up the right slot on its next turn.
        let request_model = role_model_map.model_for(active_role, &model_handle);
        let request_body = build_role_request_body(
            request_model,
            &role_messages,
            &role_tools,
            per_turn_max,
            &profile,
            force_tool,
        );
        let req_started = Instant::now();
        let resp = match post_chat_completion(runner, &request_body).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    problem = %problem_id,
                    role = active_role.id(),
                    error = %e,
                    "native_runner: role chat completion failed"
                );
                // Capture the failed request so replay can reproduce
                // it offline. Persist error text as the response body.
                request_records.push(ChatRequestRecord {
                    turn,
                    role: Some(active_role.id().to_string()),
                    request: request_body,
                    response: json!({"error": e}),
                    elapsed_ms: req_started.elapsed().as_millis() as u64,
                });
                break 'outer ExitReason::Crashed {
                    stderr_tail: format!("chat completion ({}): {e}", active_role.id()),
                };
            }
        };
        let req_elapsed_ms = req_started.elapsed().as_millis() as u64;
        request_records.push(ChatRequestRecord {
            turn,
            role: Some(active_role.id().to_string()),
            request: request_body,
            response: resp.clone(),
            elapsed_ms: req_elapsed_ms,
        });

        raw_lines.push(serde_json::to_string(&resp).unwrap_or_default());

        if let Some(u) = resp.get("usage") {
            tokens.input = u
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(tokens.input);
            tokens.output = tokens.output.saturating_add(
                u.get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            );
        }

        let choices = resp.get("choices").and_then(|v| v.as_array());
        let msg = choices
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .cloned();
        let Some(msg) = msg else {
            tracing::warn!(
                problem = %problem_id,
                role = active_role.id(),
                "native_runner: response had no choices.message"
            );
            break 'outer ExitReason::Crashed {
                stderr_tail: "response missing choices[0].message".into(),
            };
        };

        let assistant_text = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !assistant_text.is_empty() {
            final_assistant_text = assistant_text.clone();
        }

        let tool_calls = msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            empty_tool_call_streak = empty_tool_call_streak.saturating_add(1);
            tracing::info!(
                problem = %problem_id,
                role = active_role.id(),
                turn,
                empty_streak = empty_tool_call_streak,
                "native_runner: assistant turn had no tool calls"
            );
            if empty_tool_call_streak >= EMPTY_TOOL_CALL_STREAK_THRESHOLD {
                tracing::warn!(
                    problem = %problem_id,
                    empty_streak = empty_tool_call_streak,
                    threshold = EMPTY_TOOL_CALL_STREAK_THRESHOLD,
                    "native_runner: empty_tool_call_streak kill — model emitted {} content-only turns",
                    empty_tool_call_streak,
                );
                break 'outer ExitReason::NoProgress {
                    consecutive_tool_calls: empty_tool_call_streak,
                    threshold: EMPTY_TOOL_CALL_STREAK_THRESHOLD,
                };
            }
            // Implicit transition rule fires.
            let next = transition_after(active_role, TransitionTrigger::NoToolCall);
            match next {
                NextRole::Terminate => break 'outer ExitReason::Completed,
                NextRole::Stay => {
                    // Loop continues in same role; if it keeps
                    // emitting no tool calls the no_progress
                    // detector will fire below.
                }
                NextRole::Flip(r) => {
                    role_dossier.note_yield(
                        active_role,
                        format!("{} produced no tool call", active_role.id()),
                    );
                    active_role = r;
                    force_first_tool_pending = profile_for(r, scale).forced_first_tool;
                    role_chat_history.clear();
                    tracing::debug!(
                        problem = %problem_id,
                        to_role = r.id(),
                        "native_runner: chat-history reset on no-tool-call flip"
                    );
                }
            }
        }

        // Per-turn assistant message we'll persist into chat history
        // iff we stay in the active role at end-of-turn. OpenAI shape:
        // the assistant message carries content + tool_calls; tool
        // result messages reference each tool_call id. The daemon's
        // strict deserializer requires `content` to be present on
        // assistant messages even when the model emitted only
        // tool_calls — use empty string as the canonical "no prose
        // this turn" sentinel.
        let mut assistant_for_history = json!({
            "role": "assistant",
            "content": assistant_text,
        });
        if let Some(tc) = msg.get("tool_calls") {
            assistant_for_history["tool_calls"] = tc.clone();
        }
        let mut this_turn_tool_results: Vec<Value> = Vec::new();
        // Kinds dispatched this turn — read by the no-progress
        // exemption below (a read is not an attempt to mutate).
        let mut this_turn_kinds: Vec<PrimitiveKind> = Vec::new();

        // Tool calls non-empty — reset the EmptyToolCallStreak
        // detector. The model is actively driving the workdir.
        empty_tool_call_streak = 0;

        // Process tool calls. Each call: translate, dispatch,
        // append result, possibly transition.
        let mut transitioned_this_turn = false;
        for tc in &tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
            let name = fn_obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_raw = fn_obj
                .get("arguments")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    fn_obj
                        .get("arguments")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_string())
                });
            let args_value: Value = serde_json::from_str(&args_raw).unwrap_or(Value::Null);
            let args_preview = args_raw.chars().take(256).collect::<String>();

            let outcome = adapter.translate(&name, &args_value);
            let canonical_kind = outcome.canonical_kind();
            tool_calls_record.push(ToolCallRecord {
                turn,
                tool: name.clone(),
                args_preview,
                ok: matches!(outcome, TranslateOutcome::Canonical { .. }),
                canonical_kind,
            });

            // Allowed-primitive enforcement: if the model emitted a
            // tool not in the active role's subset, record but don't
            // dispatch. (The model will see a tool result indicating
            // the call was rejected and can re-decide.)
            let allowed = canonical_kind
                .map(|k| profile.allowed_primitives.contains(&k))
                .unwrap_or(false);
            if !allowed {
                tracing::warn!(
                    problem = %problem_id,
                    role = active_role.id(),
                    tool = %name,
                    "native_runner: tool not in role's allowed subset; recorded but not dispatched"
                );
                role_dossier.push_outcome(
                    active_role,
                    canonical_kind.unwrap_or(PrimitiveKind::AgentDone),
                    format!(
                        "rejected: `{}` not allowed in {} role",
                        name,
                        active_role.id()
                    ),
                );
                let allowed_list: Vec<String> = profile
                    .allowed_primitives
                    .iter()
                    .map(|k| format!("`{}`", k.id()))
                    .collect();
                let stdout_tail = format!(
                    "error: tool `{name}` rejected\n  \
                     = reason: the `{}` role does not have access to `{name}`.\n  \
                     = help: this role can use: {}. If you need a different \
                     tool, hand off to the appropriate role first \
                     (e.g. `handoff_to_evaluator` to run `build`/`smoke`, \
                     `handoff_to_implementer` to run `write_file`).",
                    active_role.id(),
                    allowed_list.join(", "),
                );
                let body = json!({
                    "ok": false,
                    "stdout_tail": stdout_tail,
                    "rejected": "tool_not_allowed_in_role",
                    "tool": name,
                    "role": active_role.id(),
                })
                .to_string();
                this_turn_tool_results.push(tool_result_message(&id, &body));
                continue;
            }

            // Dispatch via canonical executor (or special-case
            // agent_done which terminates).
            let canonical = match outcome {
                TranslateOutcome::Canonical { canonical, .. } => canonical,
                TranslateOutcome::Unrecognized {
                    tool_name,
                    args_summary,
                    reason,
                } => {
                    let stdout_tail = format!(
                        "error: tool `{tool_name}` arguments did not parse\n  \
                         = reason: {reason}\n  \
                         = args (truncated): {args_summary}\n  \
                         = help: re-emit `{tool_name}` with arguments matching its \
                         parameter schema. Common causes: missing required fields, \
                         wrong types, or escaped JSON inside JSON."
                    );
                    let body = json!({
                        "ok": false,
                        "stdout_tail": stdout_tail,
                        "rejected": "arguments_did_not_parse",
                        "tool": tool_name,
                    })
                    .to_string();
                    this_turn_tool_results.push(tool_result_message(&id, &body));
                    continue;
                }
                TranslateOutcome::Unknown { tool_name } => {
                    let stdout_tail = format!(
                        "error: tool `{tool_name}` is not in the canonical primitive set\n  \
                         = help: use one of the tools listed in the `tools` array. \
                         The canonical primitives are: write_file, build, smoke, \
                         agent_done, agent_plan, handoff_to_evaluator, \
                         handoff_to_implementer."
                    );
                    let body = json!({
                        "ok": false,
                        "stdout_tail": stdout_tail,
                        "rejected": "unknown_tool",
                        "tool": tool_name,
                    })
                    .to_string();
                    this_turn_tool_results.push(tool_result_message(&id, &body));
                    continue;
                }
            };
            let kind = canonical_kind.unwrap();
            this_turn_kinds.push(kind);

            // NOTE: write/verify counters used to live HERE (before
            // dispatch). That double-counted writes the pre-write
            // syntax check would later reject — a model retrying
            // bad content 3× hit write_thrash even though zero
            // bytes had landed on disk. Tracking now happens AFTER
            // dispatch, gated on `Ok` from the executor — see below.
            //
            // AgentDone still short-circuits here: it doesn't
            // dispatch (no work to do, just terminates the run).

            if matches!(kind, PrimitiveKind::AgentDone) {
                tracing::info!(
                    problem = %problem_id,
                    role = active_role.id(),
                    "native_runner: model emitted `agent_done`"
                );
                break 'outer ExitReason::Completed;
            }

            // Per-edit snapshot: capture workdir BEFORE any mutating
            // dispatch so we can rollback on regression. The
            // snapshot is the "known good before this edit" — used
            // only when the next smoke shows a previously-passing
            // test now failing.
            let is_edit = matches!(
                kind,
                PrimitiveKind::WriteFile
                    | PrimitiveKind::PatchFile
                    | PrimitiveKind::ReplaceFunction
            );
            let pending_snapshot = if is_edit {
                Some(snapshot_workdir(workdir.path()))
            } else {
                None
            };

            // Special-case the transition primitives: they don't
            // execute work, just update the dossier and trigger
            // transition. We still call into the executor to keep
            // telemetry uniform.
            let result = match registry.dispatch(&exec_ctx, &canonical).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        problem = %problem_id,
                        role = active_role.id(),
                        tool = %name,
                        error = %e,
                        "native_runner: dispatch failed"
                    );
                    role_dossier.push_outcome(active_role, kind, format!("error: {e}"));
                    // Render in cargo-shape texture so the model sees
                    // a structured, actionable error instead of the
                    // bare enum string. Per ARCH §0.1 (glassbox):
                    // model-to-model communication is itself part of
                    // the system surface that must be legible.
                    let stdout_tail = e.render_for_agent();
                    let body = json!({
                        "ok": false,
                        "stdout_tail": stdout_tail,
                        "rejected": "executor_error",
                        "tool": name,
                    })
                    .to_string();
                    this_turn_tool_results.push(tool_result_message(&id, &body));
                    // Windowed sticky-retry: push this signature onto
                    // the recent-rejections window (capped at
                    // STICKY_RETRY_WINDOW). If ≥ STICKY_RETRY_THRESHOLD
                    // of the entries in the window share a signature,
                    // the model is stuck on the same failure mode
                    // even if it's making other (irrelevant) tool
                    // calls in between. Fire StickyRetry.
                    let sig = e.sticky_signature();
                    if recent_rejection_signatures.len() == STICKY_RETRY_WINDOW {
                        recent_rejection_signatures.pop_front();
                    }
                    recent_rejection_signatures.push_back(sig.clone());
                    let matches_in_window = recent_rejection_signatures
                        .iter()
                        .filter(|s| **s == sig)
                        .count();
                    // Recovery-escalation: ONE rejection before the
                    // sticky-kill, if the Implementer is stuck re-emitting
                    // a rejected SPLICE edit (patch_file / replace_function)
                    // at the same site, arm a forced full rewrite. A
                    // write_file rewrite has no line-range/body splice
                    // contract to violate and gives a fresh regeneration
                    // that won't reproduce a one-off formatting glitch —
                    // generic over the defect (interface mismatch OR model
                    // glitch). The kill below remains as last resort if even
                    // the rewrite keeps failing.
                    if matches_in_window == STICKY_RETRY_THRESHOLD - 1
                        && matches!(active_role, Role::Implementer)
                        && matches!(
                            kind,
                            PrimitiveKind::PatchFile | PrimitiveKind::ReplaceFunction
                        )
                    {
                        force_full_rewrite_pending = true;
                        tracing::info!(
                            problem = %problem_id,
                            primitive = kind.id(),
                            signature = %sig,
                            "native_runner: recovery-escalation armed — next Implementer turn forced to write_file (full rewrite)"
                        );
                    }
                    if matches_in_window >= STICKY_RETRY_THRESHOLD {
                        tracing::warn!(
                            problem = %problem_id,
                            role = active_role.id(),
                            primitive = kind.id(),
                            signature = %sig,
                            matches = matches_in_window,
                            window = STICKY_RETRY_WINDOW,
                            "native_runner: sticky-retry kill — signature recurred in window"
                        );
                        break 'outer ExitReason::StickyRetry {
                            primitive: kind.id().to_string(),
                            repeats: matches_in_window as u32,
                        };
                    }
                    continue;
                }
            };
            // Successful dispatch does NOT clear the rejection window.
            // The window naturally ages as new rejections push old
            // ones off; a model that alternates `build (ok) → smoke
            // (timeout)` is just as stuck as one that emits
            // back-to-back identical errors, so we measure both.
            // Edit succeeded → keep the pre-edit snapshot live so
            // the next smoke can rollback on regression.
            if let Some(snap) = pending_snapshot {
                pre_edit_snapshot = Some(snap);
            }
            // Persist the real result content as a tool message so
            // the next turn (within this role) sees the full payload
            // — stdout_tail, pass/fail counts, etc. — instead of just
            // the dossier's 200-char summary.
            let result_body = serde_json::to_string(&result.payload).unwrap_or_default();
            this_turn_tool_results.push(tool_result_message(&id, &result_body));

            // Track thrash + writes-since-verify ONLY for writes
            // that actually landed on disk (Ok from the executor).
            // Pre-write syntax check rejections take the Err branch
            // above and never reach here — so a model retrying bad
            // content N× doesn't trip the write-thrash detector,
            // because no bytes hit disk. PatchFile is symmetric
            // with WriteFile here: a successful patch is the same
            // class of workdir mutation, and repeated patches to
            // the same file without verification are the same kind
            // of thrash.
            if matches!(
                kind,
                PrimitiveKind::WriteFile
                    | PrimitiveKind::PatchFile
                    | PrimitiveKind::ReplaceFunction
            ) {
                let path = match &canonical {
                    commonwealth_agent_tools::Primitive::WriteFile(args) => {
                        Some(args.path.as_str())
                    }
                    commonwealth_agent_tools::Primitive::PatchFile(args) => {
                        Some(args.path.as_str())
                    }
                    commonwealth_agent_tools::Primitive::ReplaceFunction(args) => {
                        Some(args.path.as_str())
                    }
                    _ => None,
                };
                let signal = thrash.observe_write(path);
                role_dossier.on_write();
                if let ThrashSignal::Kill { same_path_writes } = signal {
                    tracing::warn!(
                        problem = %problem_id,
                        role = active_role.id(),
                        same_path_writes,
                        threshold = SAME_PATH_WRITE_THRESHOLD,
                        "native_runner: write_thrash kill"
                    );
                    break 'outer ExitReason::WriteThrash {
                        consecutive_writes: same_path_writes,
                        threshold: SAME_PATH_WRITE_THRESHOLD,
                    };
                }
            }
            if matches!(kind, PrimitiveKind::Build | PrimitiveKind::Smoke) {
                thrash.observe_verify();
                role_dossier.on_verify();
            }

            // For Build/Smoke: also stash the stdout_tail in the
            // dossier so the next Implementer (after a
            // handoff_to_implementer) sees the compiler output even
            // though role_chat_history clears on flip. Also feed the
            // VerifyStuck detector — same failing output 3x → kill.
            if matches!(kind, PrimitiveKind::Build | PrimitiveKind::Smoke) {
                let tail = result
                    .payload
                    .get("stdout_tail")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let ok = result
                    .payload
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // Rollback gate: compare smoke's failed_names to
                // the prior smoke's failed_names. Any test newly
                // failing (was passing before) means the most
                // recent edit regressed it → restore the pre-edit
                // snapshot and seed the dossier with an explicit
                // "your edit broke X; try a different approach."
                // Run only on Smoke (Build doesn't enumerate tests).
                if matches!(kind, PrimitiveKind::Smoke) {
                    let parsed = crate::witness::test_result_parser::parse_pytest_text(tail);
                    let new_failed: HashSet<String> = parsed.failed_names.iter().cloned().collect();
                    let regressed: Vec<String> = match &last_failed_set {
                        Some(prev) => new_failed.difference(prev).cloned().collect(),
                        None => Vec::new(),
                    };
                    if !regressed.is_empty() && pre_edit_snapshot.is_some() {
                        let snap = pre_edit_snapshot.take().unwrap();
                        if let Err(e) = restore_workdir(workdir.path(), &snap) {
                            tracing::warn!(
                                problem = %problem_id,
                                error = %e,
                                "native_runner: rollback failed to restore workdir"
                            );
                        } else {
                            let sample: Vec<&str> =
                                regressed.iter().take(3).map(|s| s.as_str()).collect();
                            let msg = format!(
                                "REGRESSION: your last edit broke {} previously-passing test(s) ({}). Workdir reverted to pre-edit state. Try a different approach for this fix.",
                                regressed.len(),
                                sample.join(", ")
                            );
                            role_dossier.set_diagnosis(msg.clone());
                            tracing::info!(
                                problem = %problem_id,
                                role = active_role.id(),
                                regressed_count = regressed.len(),
                                "native_runner: rolled back edit after regression"
                            );
                        }
                        // Don't update last_failed_set since we rolled back.
                    } else {
                        last_failed_set = Some(new_failed);
                        pre_edit_snapshot = None; // edit accepted
                    }
                }
                role_dossier.record_verification(kind, ok, tail);
                if let VerifySignal::Kill { hash_repeats } = verify_stuck.observe(ok, tail) {
                    tracing::warn!(
                        problem = %problem_id,
                        role = active_role.id(),
                        primitive = kind.id(),
                        hash_repeats,
                        threshold = VERIFY_STUCK_THRESHOLD,
                        "native_runner: verify_stuck kill — same failing verification output {} times",
                        hash_repeats,
                    );
                    break 'outer ExitReason::VerifyStuck {
                        hash_repeats,
                        threshold: VERIFY_STUCK_THRESHOLD,
                    };
                }
            }

            // HandoffToImplementer increments the cycle counter
            // (Evaluator concluding "Implementer retry"). Cap closes
            // L4/L7/L17 unbounded-alternation classes. Also tracks
            // replan threshold (B): after `REPLAN_THRESHOLD` handoffs
            // without an intervening agent_plan, the transition rule
            // below routes the next handoff back to Planner instead
            // of Implementer.
            if matches!(kind, PrimitiveKind::HandoffToImplementer) {
                cycles_since_last_plan = cycles_since_last_plan.saturating_add(1);
                if let CycleSignal::Kill { cycles } =
                    handoff_cycles.observe_handoff_to_implementer()
                {
                    tracing::warn!(
                        problem = %problem_id,
                        cycles,
                        cap = HANDOFF_CYCLE_CAP,
                        "native_runner: cycle_limit kill — Implementer↔Evaluator alternated {} times",
                        cycles,
                    );
                    break 'outer ExitReason::CycleLimit {
                        cycles,
                        cap: HANDOFF_CYCLE_CAP,
                    };
                }
            }
            // AgentPlan resets the replan counter — Planner just emitted
            // a fresh plan, so subsequent handoffs start from cycle 0.
            if matches!(kind, PrimitiveKind::AgentPlan) {
                cycles_since_last_plan = 0;
            }

            // Special updates from transition primitives.
            match (&canonical, kind) {
                (
                    commonwealth_agent_tools::Primitive::AgentPlan(args),
                    PrimitiveKind::AgentPlan,
                ) => {
                    role_dossier.set_plan(args.plan.clone());
                    if let Some(items) = args.pseudocode.clone() {
                        role_dossier.set_pseudocode(items);
                    }
                }
                (
                    commonwealth_agent_tools::Primitive::HandoffToImplementer(args),
                    PrimitiveKind::HandoffToImplementer,
                ) => {
                    role_dossier.set_diagnosis(args.diagnosis.clone());
                }
                (
                    commonwealth_agent_tools::Primitive::HandoffToEvaluator(args),
                    PrimitiveKind::HandoffToEvaluator,
                ) => {
                    // The "what_you_changed" becomes the
                    // Evaluator's last-action context.
                    role_dossier.note_yield(active_role, args.what_you_changed.clone());
                }
                _ => {}
            }

            let summary = summarize_for_dossier(kind, &result);
            role_dossier.push_outcome(active_role, kind, summary);

            // §C per-tenure turn-cap detector. Replaces the deleted
            // same-primitive counter. Fires only on pathological
            // non-yielding tenures — a model that calls the same
            // primitive 3× while making real progress (workdir
            // changes, dossier accumulates) is no longer cut off
            // here; the workdir-hash no-progress detector + the
            // §B grammar gate + the cycle-limit detector handle
            // legitimate stuck-state honestly.
            tool_calls_in_tenure = tool_calls_in_tenure.saturating_add(1);
            let cap = per_tenure_turn_cap(active_role);
            if tool_calls_in_tenure > cap {
                tracing::warn!(
                    problem = %problem_id,
                    role = active_role.id(),
                    tool_calls = tool_calls_in_tenure,
                    cap,
                    "native_runner: per-tenure turn cap exceeded; forcing exit"
                );
                break 'outer ExitReason::RoleTurnCap {
                    role: active_role.id().to_string(),
                    tool_calls: tool_calls_in_tenure,
                    cap,
                };
            }

            // Transition. Standard rules from `transition_after`; the
            // role-aware runner additionally overrides for replan: if
            // HandoffToImplementer fires with the cycle counter at or
            // above REPLAN_THRESHOLD, route back to Planner instead
            // so the model can revise the plan against the failure
            // dossier. Counter resets on the next AgentPlan dispatch
            // (already wired above).
            let next_raw = transition_after(active_role, TransitionTrigger::Primitive(kind));
            let next = if matches!(kind, PrimitiveKind::HandoffToImplementer)
                && cycles_since_last_plan >= REPLAN_THRESHOLD
                && matches!(next_raw, NextRole::Flip(Role::Implementer))
            {
                tracing::info!(
                    problem = %problem_id,
                    cycles_since_last_plan,
                    threshold = REPLAN_THRESHOLD,
                    "native_runner: replan triggered — routing handoff_to_implementer back to Planner instead"
                );
                NextRole::Flip(Role::Planner)
            } else {
                next_raw
            };
            match next {
                NextRole::Terminate => break 'outer ExitReason::Completed,
                NextRole::Stay => {}
                NextRole::Flip(r) => {
                    // Note the yield BEFORE flipping so the next
                    // role's dossier sees the prior role's last
                    // action.
                    if role_dossier.last_action_summary.is_none()
                        || !matches!(kind, PrimitiveKind::HandoffToEvaluator)
                    {
                        role_dossier.note_yield(
                            active_role,
                            format!("{} called {}", active_role.id(), kind.id()),
                        );
                    }
                    active_role = r;
                    force_first_tool_pending = profile_for(r, scale).forced_first_tool;
                    tool_calls_in_tenure = 0;
                    transitioned_this_turn = true;
                    role_chat_history.clear();
                    tracing::debug!(
                        problem = %problem_id,
                        to_role = r.id(),
                        triggering_primitive = kind.id(),
                        "native_runner: chat-history reset on role flip"
                    );
                    // Clear the diagnosis once the receiving role
                    // has been set up (the next request renders
                    // it; subsequent requests in the same role
                    // shouldn't re-show it).
                    if matches!(kind, PrimitiveKind::HandoffToEvaluator) {
                        // Evaluator's diagnosis is only what
                        // arrives via HandoffToImplementer; clear
                        // any stale one.
                        role_dossier.clear_diagnosis();
                    }
                    break;
                }
            }
        }

        // Persist this turn's assistant + tool_result messages into
        // per-tenure chat history iff we stayed in the active role.
        // On Flip the role_chat_history was cleared above; the next
        // role's first turn starts fresh.
        if !transitioned_this_turn && !tool_calls.is_empty() {
            role_chat_history.push(assistant_for_history);
            role_chat_history.extend(this_turn_tool_results);
        }

        // No-progress detector: skip while in Planner (planner
        // doesn't write by design, so workdir-hash stability is
        // expected; firing here would kill every Planner-only
        // turn). Only meaningful in Implementer/Evaluator where
        // an unchanged workdir really does signal stuck-state.
        //
        // A READ is exempt for the same reason the Planner is: it is
        // not an attempt to mutate, so an unchanged workdir after one
        // carries no signal. This only matters at Repository scale,
        // where `inspect_workdir` is the role's way of seeing the code
        // at all — measured 2026-08-18, the 27B spent all 8 calls
        // reading and was killed for "no progress" while doing exactly
        // what the prompt told it to do. Runaway reading is still
        // bounded by the token and wall caps.
        let read_only_turn = !this_turn_kinds.is_empty()
            && this_turn_kinds
                .iter()
                .all(|k| matches!(k, PrimitiveKind::InspectWorkdir));
        if !matches!(active_role, Role::Planner) && !read_only_turn {
            let new_hash = hash_workdir(workdir.path());
            if new_hash == last_workdir_hash {
                consecutive_no_progress = consecutive_no_progress.saturating_add(1);
            } else {
                consecutive_no_progress = 0;
                last_workdir_hash = new_hash;
            }
            if consecutive_no_progress >= NO_PROGRESS_TOOL_CALLS_THRESHOLD {
                break 'outer ExitReason::NoProgress {
                    consecutive_tool_calls: consecutive_no_progress,
                    threshold: NO_PROGRESS_TOOL_CALLS_THRESHOLD,
                };
            }
        }
        // Planner-specific budget: cap the number of consecutive
        // inspect-only turns Planner can take before we force a
        // transition. The Planner profile's forced_first_tool
        // fires `inspect_workdir` once; subsequent turns are
        // free choice but the model has been observed looping on
        // inspect_workdir under uncertainty. After 3 inspect-only
        // turns we force-transition to Implementer with an
        // empty plan — the no-plan case still produces an
        // honest measurement of what the Implementer can do
        // without guidance.
        if matches!(active_role, Role::Planner)
            && total_role_calls >= 3
            && role_dossier.plan.is_none()
        {
            tracing::info!(
                problem = %problem_id,
                role = "planner",
                "native_runner: Planner exceeded inspect budget without emitting agent_plan; force-transitioning to Implementer with empty plan"
            );
            role_dossier.set_plan(
                "(Planner did not emit a plan within budget. Implementer: write the solution \
                 directly. Spec is in your user message; workdir contents are listed there too.)"
                    .to_string(),
            );
            role_dossier.note_yield(
                Role::Planner,
                "planner exhausted inspect budget; no plan emitted".to_string(),
            );
            active_role = Role::Implementer;
            force_first_tool_pending = profile_for(Role::Implementer, scale).forced_first_tool;
            total_role_calls = 0;
            role_chat_history.clear();
            tracing::debug!(
                problem = %problem_id,
                "native_runner: chat-history reset on planner force-transition"
            );
        }
        let _ = transitioned_this_turn;
    };

    let wall_ms = started.elapsed().as_millis() as u64;
    let role_model_map_used = if role_model_map.is_empty() {
        None
    } else {
        Some(role_model_map)
    };
    Ok(AgentRunArtifact {
        workdir,
        tokens,
        wall_ms,
        exit_reason,
        tool_calls: tool_calls_record,
        stderr_tail: String::new(),
        final_assistant_text,
        raw_stdout_lines: cap_raw_lines(raw_lines),
        request_records,
        role_model_map_used,
    })
}

/// Filter the canonical descriptor set down to the active role's
/// allowed primitives. The model literally cannot call a tool that
/// isn't in this list — the OpenAI API rejects unknown tool names.
fn filter_descriptors(adapter: &native_adapter::Adapter, profile: &RoleProfile) -> Vec<Value> {
    filter_descriptors_for(adapter, &profile.allowed_primitives)
}

/// Filter the canonical descriptor set against an explicit primitive
/// list — the structural enforcement point. Used by the default
/// profile path (`filter_descriptors`) and by §B's conditional
/// restriction (`EVALUATOR_TERMINATING_SUBSET` after a passing
/// smoke).
fn filter_descriptors_for(
    adapter: &native_adapter::Adapter,
    allowed: &[PrimitiveKind],
) -> Vec<Value> {
    let all = adapter.tool_descriptors();
    let allowed_ids: Vec<&'static str> = allowed.iter().map(|p| p.id()).collect();
    all.into_iter()
        .filter(|d| {
            let name = d
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            allowed_ids.contains(&name)
        })
        .collect()
}

fn build_role_messages(
    role: Role,
    profile: &RoleProfile,
    dossier: &RoleDossier,
    initial_user_msg: &str,
    role_chat_history: &[Value],
) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    // System message = profile prompt + rendered dossier.
    let rendered_dossier = dossier.render(role);
    let system_content = if rendered_dossier.is_empty() {
        profile.system_prompt.clone()
    } else {
        format!("{}\n\n{}", profile.system_prompt, rendered_dossier)
    };
    messages.push(json!({"role": "system", "content": system_content}));
    messages.push(json!({"role": "user", "content": initial_user_msg}));
    // Per-tenure chat history (assistant + tool result pairs from
    // prior turns within the active role). Empty on the first turn
    // of a role; populated turn-by-turn while the role stays.
    messages.extend(role_chat_history.iter().cloned());
    messages
}

/// Build the chat-completion request body for the active role.
/// Pure: no I/O. Returned body is captured into `ChatRequestRecord`
/// so the `replay` subcommand can re-send it with overrides.
fn build_role_request_body(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    max_tokens: u64,
    profile: &RoleProfile,
    force_tool: Option<PrimitiveKind>,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "stream": false,
        "max_tokens": max_tokens,
    });
    if let Some(force) = force_tool {
        body["tool_choice"] = json!({
            "type": "function",
            "function": {"name": force.id()},
        });
    } else {
        body["tool_choice"] = json!("auto");
    }
    if let Some(t) = profile.sampling.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = profile.sampling.top_p {
        body["top_p"] = json!(p);
    }
    body
}

/// POST a pre-built request body to the daemon. Returns the parsed
/// response on success, or `Err(text)` on HTTP / parse failure.
async fn post_chat_completion(runner: &NativeRunner, body: &Value) -> Result<Value, String> {
    let url = format!("{}/chat/completions", runner.provider_url);
    let resp = runner
        .http
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "daemon {status}: {}",
            text.chars().take(500).collect::<String>()
        ));
    }
    serde_json::from_str(&text).map_err(|e| {
        format!(
            "parse: {e} (body: {})",
            text.chars().take(500).collect::<String>()
        )
    })
}

// ── HTTP transport ───────────────────────────────────────────────

async fn send_chat_completion(
    runner: &NativeRunner,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    max_tokens: u64,
) -> Result<Value, String> {
    let body = json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "stream": false,
        "max_tokens": max_tokens,
    });
    let url = format!("{}/chat/completions", runner.provider_url);
    let resp = runner
        .http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "daemon {status}: {}",
            text.chars().take(500).collect::<String>()
        ));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| {
        format!(
            "parse: {e} (body: {})",
            text.chars().take(500).collect::<String>()
        )
    })?;
    Ok(v)
}

// ── helpers ──────────────────────────────────────────────────────

fn system_message() -> Value {
    json!({
        "role": "system",
        "content": "You are a coding agent operating in a workdir. Use the provided tools to inspect, write, build, and verify code. When all tests pass, call `agent_done` with a one-sentence reason. Each tool call returns a structured JSON result; read the result before deciding the next call.",
    })
}

fn user_message(content: &str) -> Value {
    json!({
        "role": "user",
        "content": content,
    })
}

fn tool_result_message(tool_call_id: &str, content: &str) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": content,
    })
}

fn format_initial_prompt(workdir: &Path, problem_prompt: &str) -> String {
    format_initial_prompt_with_promoted(
        workdir,
        problem_prompt,
        &Default::default(),
        crate::runner::DEFAULT_ANCHOR_BUDGET_LINES,
        crate::runner::DEFAULT_WORKDIR_LISTING_DEPTH,
    )
}

/// Variant that promotes named declarations from outline → inline.
/// When the Planner emits pseudocode naming specific
/// functions/classes, those declarations get their full body
/// rendered in the anchor block (instead of just the signature)
/// so the Implementer can see exact body content before patching.
/// Closes the "model patches a wider range than its pseudocode
/// specified, truncating the function" class observed on 4.2
/// v-pseudo 2026-05-23.
fn format_initial_prompt_with_promoted(
    workdir: &Path,
    problem_prompt: &str,
    promoted_names: &std::collections::HashSet<String>,
    anchor_budget: usize,
    listing_depth: usize,
) -> String {
    let state = summarize_workdir_to_depth(workdir, listing_depth);
    let anchors = render_workdir_anchors_with_promoted(workdir, promoted_names, anchor_budget);
    format!(
        "## Workdir state (factual, current state of `.`)\n{state}\n{anchors}\n---\n\n{problem_prompt}"
    )
}

/// Extract identifier-like words from each pseudocode entry.
/// Heuristic: split on non-`[A-Za-z0-9_]` characters; keep tokens
/// that start with an alpha/underscore. The model writes pseudocode
/// like `"tokenize (lines 75-90): Replace..."` and `"Parser.parse_pow
/// (lines 226-236): ..."`. We extract `tokenize`, `Parser`,
/// `parse_pow`, `lines`, `Replace`, etc. — over-extraction is fine;
/// only words that match actual declaration names in the source
/// trigger promotion downstream.
fn extract_referenced_names(pseudocode: &[String]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for entry in pseudocode {
        for word in entry.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if word.is_empty() {
                continue;
            }
            let first = word.chars().next().unwrap();
            if first.is_ascii_alphabetic() || first == '_' {
                names.insert(word.to_string());
            }
        }
    }
    names
}

/// Maximum number of lines per source file to render inline. Files
/// above this cap get a function-signature outline instead of full
/// content.
///
/// Bumped 150 → 500 on 2026-05-24 after the 4.2 isolation probe
/// v2 (one-shot, full file shown each call) lifted Darwin's per-bug
/// pass rate from 32% to 60% over the v1 forced-function-rewrite
/// shape. The discriminator was inline body content: with line-
/// numbered body visible, the model emits accurate `patch_lines`
/// ranges and mirrors existing indentation/identifiers; in outline
/// mode it has to reconstruct body content from memory and either
/// hallucinates the wrong lines or fabricates names. The 4.2
/// evaluator.py is 373 lines and is the canonical hot path; 500
/// covers it plus typical real-world Python files.
const MAX_FILE_LINES_INLINE: usize = 500;

/// Hard cap on the total number of source-content lines rendered
/// across all anchored files. Prevents many-file workdirs from
/// blowing the prompt budget.
///
/// Bumped 500 → 1200 on 2026-05-24 to admit the workdir state
/// after MAX_FILE_LINES_INLINE rose to 500: a typical scaffolded
/// problem now has main-source-file (~400 lines) + test file
/// (~200 lines) + headroom for additional helpers, all inline.
/// At ~80 chars/line × 1200 lines × 0.25 tokens/char ≈ 24K input
/// tokens of anchors — fits comfortably in the 32K context with
/// system + dossier + chat history.
const MAX_TOTAL_ANCHOR_LINES: usize = 1200;

/// Render line-numbered content for source files in the workdir so
/// the Implementer can use `patch_file` against ground-truth line
/// numbers (and so the Evaluator can quote specific lines in its
/// diagnoses). Skips tests/, build output, vendored deps, and VCS
/// metadata — the model's primary edit target is its own source
/// files, not infrastructure it shouldn't touch.
///
/// Per-file rendering:
/// - ≤ `MAX_FILE_LINES_INLINE` lines: full line-numbered content
///   inside a fenced block.
/// - > `MAX_FILE_LINES_INLINE` lines: function/class signatures
///   > only, each prefixed with its line number. Lets the model
///   > address regions by signature line without paying for every
///   > line of body.
///
/// Returns an empty string when no source files are present (e.g.
/// FromScratch tier before the first write).
fn render_workdir_anchors(workdir: &Path) -> String {
    render_workdir_anchors_with_promoted(
        workdir,
        &Default::default(),
        crate::runner::DEFAULT_ANCHOR_BUDGET_LINES,
    )
}

/// Extract the declaration NAME from a top-level line.
/// `def foo(...)` → `Some("foo")`, `class Bar:` → `Some("Bar")`,
/// `pub fn baz` → `Some("baz")`, etc. Returns None when the line
/// isn't a recognized declaration.
fn decl_name(line: &str) -> Option<&str> {
    let s = line.trim_start();
    let after = if let Some(r) = s.strip_prefix("async def ") {
        r
    } else if let Some(r) = s.strip_prefix("def ") {
        r
    } else if let Some(r) = s.strip_prefix("class ") {
        r
    } else if let Some(r) = s.strip_prefix("pub fn ") {
        r
    } else if let Some(r) = s.strip_prefix("pub(crate) fn ") {
        r
    } else if let Some(r) = s.strip_prefix("fn ") {
        r
    } else if let Some(r) = s.strip_prefix("pub struct ") {
        r
    } else if let Some(r) = s.strip_prefix("pub enum ") {
        r
    } else if let Some(r) = s.strip_prefix("pub trait ") {
        r
    } else if let Some(r) = s.strip_prefix("export default function ") {
        r
    } else if let Some(r) = s.strip_prefix("export function ") {
        r
    } else if let Some(r) = s.strip_prefix("function ") {
        r
    } else {
        s.strip_prefix("export class ")?
    };
    let end = after
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(after.len());
    if end == 0 {
        None
    } else {
        Some(&after[..end])
    }
}

fn render_workdir_anchors_with_promoted(
    workdir: &Path,
    promoted_names: &std::collections::HashSet<String>,
    anchor_budget: usize,
) -> String {
    if anchor_budget == 0 {
        // Repository-scale workdir: enumeration is noise, not orientation.
        return String::new();
    }
    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    collect_source_files(workdir, workdir, 0, &mut files);
    if files.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Current source files (line-numbered, factual)\n");
    let mut total_lines_rendered: usize = 0;
    for (rel, content) in files {
        if total_lines_rendered >= anchor_budget {
            out.push_str(&format!(
                "\n(further source files omitted — total anchor budget {} lines exceeded)\n",
                anchor_budget
            ));
            break;
        }
        let lines: Vec<&str> = content.lines().collect();
        out.push_str(&format!(
            "\n### `{}` ({} lines)\n```\n",
            rel.display(),
            lines.len()
        ));
        if lines.len() <= MAX_FILE_LINES_INLINE {
            let take = lines
                .len()
                .min(anchor_budget.saturating_sub(total_lines_rendered));
            for (i, l) in lines.iter().take(take).enumerate() {
                // Use `:` separator (not `| `) — pipe visually
                // suggests unified-diff format, which the model
                // then copies into patch_file's new_content,
                // emitting `| N | content` instead of raw
                // replacement Python. Observed 4.2 2026-05-23.
                out.push_str(&format!("{:>4}: {}\n", i + 1, l));
            }
            total_lines_rendered = total_lines_rendered.saturating_add(take);
        } else {
            // Outline mode — render each top-level declaration with
            // its LINE EXTENT, computed as the line range from the
            // declaration to the line BEFORE the next declaration
            // (or EOF for the last). Extents are load-bearing for
            // patch_file correctness: without them, the model
            // estimates body ranges and patches that cross
            // declaration boundaries, producing
            // syntactically-broken merges (observed 2026-05-23 on
            // 4.2 — every rejected patch targeted a range that
            // spanned multiple declarations). Extents let the model
            // patch within a single function's body with confidence.
            let decl_lines: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter_map(|(i, l)| {
                    let s = l.trim_start();
                    let is_decl = s.starts_with("def ")
                        || s.starts_with("async def ")
                        || s.starts_with("class ")
                        || s.starts_with("fn ")
                        || s.starts_with("pub fn ")
                        || s.starts_with("pub(crate) fn ")
                        || s.starts_with("pub struct ")
                        || s.starts_with("pub enum ")
                        || s.starts_with("pub trait ")
                        || s.starts_with("impl ")
                        || s.starts_with("function ")
                        || s.starts_with("export function ")
                        || s.starts_with("export default function ")
                        || s.starts_with("export class ")
                        || s.starts_with("type ")
                        || s.starts_with("interface ");
                    if is_decl {
                        Some(i)
                    } else {
                        None
                    }
                })
                .collect();
            for (k, &i) in decl_lines.iter().enumerate() {
                let start = i + 1; // 1-indexed
                let end = if k + 1 < decl_lines.len() {
                    decl_lines[k + 1] // line BEFORE next decl
                } else {
                    lines.len() // EOF (1-indexed last line)
                };
                let l = lines[i];
                // Promotion: when this declaration's name appears in
                // the Planner's pseudocode, render its FULL body
                // inline so the Implementer sees exact content
                // before patching. Closes the "model patches a
                // wider range than its pseudocode specified,
                // truncating the function" class observed on 4.2
                // v-pseudo 2026-05-23.
                let name_promoted = decl_name(l)
                    .map(|n| promoted_names.contains(n))
                    .unwrap_or(false);
                if name_promoted {
                    let body_len = end - start + 1;
                    let remaining = anchor_budget.saturating_sub(total_lines_rendered);
                    let take = body_len.min(remaining);
                    out.push_str(&format!(
                        "{:>4}: {}  [lines {}-{}]  ← PROMOTED (named in pseudocode)\n",
                        start, l, start, end
                    ));
                    for off in 1..take {
                        let ln = start + off;
                        if ln - 1 < lines.len() {
                            out.push_str(&format!("{:>4}: {}\n", ln, lines[ln - 1]));
                        }
                    }
                    total_lines_rendered = total_lines_rendered.saturating_add(take);
                } else {
                    out.push_str(&format!("{:>4}: {}  [lines {}-{}]\n", start, l, start, end));
                    total_lines_rendered = total_lines_rendered.saturating_add(1);
                }
            }
            out.push_str(&format!(
                "(outline mode — full file is {} lines. Declarations named in your pseudocode are promoted to full body inline; others show signature + line extent only. patch_file ranges should stay WITHIN one declaration's extent.)\n",
                lines.len()
            ));
        }
        out.push_str("```\n");
    }
    out
}

/// Snapshot every source file in the workdir so we can rollback if
/// the next smoke regresses. Reads the same set the anchor renderer
/// reads — language source files, not tests/build output.
fn snapshot_workdir(workdir: &Path) -> HashMap<PathBuf, String> {
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    collect_source_files(workdir, workdir, 0, &mut files);
    files.into_iter().collect()
}

/// Restore source files from a snapshot. Any file in the current
/// workdir whose path matches a snapshot entry is overwritten with
/// the snapshot's content. Files that weren't in the snapshot are
/// left as-is (the user-message workdir-state preamble still sees
/// them, and the model can choose to delete them via write_file
/// with empty content if needed).
fn restore_workdir(workdir: &Path, snapshot: &HashMap<PathBuf, String>) -> std::io::Result<()> {
    for (rel, content) in snapshot {
        let abs = workdir.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, content.as_bytes())?;
    }
    Ok(())
}

const SOURCE_EXTENSIONS: &[&str] = &[".py", ".rs", ".go", ".ts", ".tsx"];
const ANCHOR_SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "__pycache__",
    "vendor",
    "tests",
    "test",
    "dist",
    "build",
];

fn collect_source_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<(std::path::PathBuf, String)>,
) {
    if depth > 3 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let p = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if ANCHOR_SKIP_DIRS.iter().any(|s| *s == name) {
                continue;
            }
            collect_source_files(root, &p, depth + 1, out);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let path_str = p.to_string_lossy();
        if !SOURCE_EXTENSIONS.iter().any(|ext| path_str.ends_with(ext)) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&p) else {
            continue;
        };
        let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
        out.push((rel, content));
    }
}

fn summarize_workdir(workdir: &Path) -> String {
    summarize_workdir_to_depth(workdir, crate::runner::DEFAULT_WORKDIR_LISTING_DEPTH)
}

fn summarize_workdir_to_depth(workdir: &Path, max_depth: usize) -> String {
    let mut entries: Vec<String> = Vec::new();
    collect_workdir_entries(workdir, workdir, 0, max_depth, &mut entries);
    // Emitted once, by the single caller — pushing it inside the
    // recursion appends one notice per stack frame.
    if entries.len() >= MAX_WORKDIR_ENTRIES {
        entries.truncate(MAX_WORKDIR_ENTRIES);
        entries.push(format!(
            "  (listing truncated at {MAX_WORKDIR_ENTRIES} entries — use `ls`/`find` to explore)"
        ));
    }
    if entries.is_empty() {
        "(empty — the workdir contains no files. You must create Cargo.toml and src/lib.rs via the `write_file` tool.)".to_string()
    } else {
        entries.join("\n")
    }
}

/// Cap on entries in the workdir-state preamble.
///
/// This listing was unbounded until 2026-08-18, which is invisible on
/// an agent-coding scaffold (1-3 files) and fatal on a real repository:
/// measured on a pylint checkout it rendered 1,122 entries / 56,510
/// chars / ~14,127 tokens, and the resulting 39,251-token prompt was
/// rejected by a 16,000-token slot before the planner's first call.
/// A listing is orientation, not an inventory — the agent has `ls`,
/// `find` and `grep` for the rest.
const MAX_WORKDIR_ENTRIES: usize = 200;

fn collect_workdir_entries(
    root: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<String>,
) {
    if depth > max_depth || out.len() >= MAX_WORKDIR_ENTRIES {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        if out.len() >= MAX_WORKDIR_ENTRIES {
            return;
        }
        let p = entry.path();
        let rel = p
            .strip_prefix(root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string_lossy().into_owned());
        if let Ok(meta) = entry.metadata() {
            if meta.is_dir() {
                // Same skip-list the anchor renderer uses. Without it
                // this walked `.git/objects/**` and the full test tree.
                let name = entry.file_name().to_string_lossy().into_owned();
                if ANCHOR_SKIP_DIRS.iter().any(|s| *s == name) {
                    continue;
                }
                out.push(format!("  {rel}/"));
                collect_workdir_entries(root, &p, depth + 1, max_depth, out);
            } else if meta.is_file() {
                out.push(format!("  {rel}  ({} bytes)", meta.len()));
            }
        }
    }
}

fn hash_workdir(workdir: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    walk_for_hash(workdir, &mut hasher);
    hasher.finish()
}

fn walk_for_hash(dir: &Path, hasher: &mut DefaultHasher) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let p = entry.path();
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == "target" || name == ".git" {
            continue;
        }
        name.hash(hasher);
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                meta.len().hash(hasher);
                if let Ok(bytes) = std::fs::read(&p) {
                    bytes.hash(hasher);
                }
            } else if meta.is_dir() {
                walk_for_hash(&p, hasher);
            }
        }
    }
}

fn cap_raw_lines(lines: Vec<String>) -> Vec<String> {
    let mut total: usize = 0;
    let mut out: Vec<String> = Vec::new();
    for l in lines.into_iter().rev() {
        total = total.saturating_add(l.len());
        out.push(l);
        if total > STDERR_TAIL_CAP_BYTES {
            break;
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    /// The preamble must stay bounded on a repository-scale tree.
    ///
    /// Regression for 2026-08-18: `collect_workdir_entries` had no
    /// skip-list and no cap, so a real checkout rendered 1,122 entries
    /// (~14,127 tokens) and the resulting 39,251-token prompt was
    /// rejected by a 16,000-token slot before the planner's first call.
    /// A scaffold never reveals this — only a tree with depth and a
    /// `.git`/`tests` directory does.
    #[test]
    fn workdir_listing_is_bounded_on_a_repo_scale_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // 6 top-level dirs x 4 subdirs x 30 files = 720 source files,
        // plus the two directories the skip-list must exclude.
        for d in 0..6 {
            for sub in 0..4 {
                let dir = root.join(format!("pkg{d}")).join(format!("mod{sub}"));
                std::fs::create_dir_all(&dir).unwrap();
                for f in 0..30 {
                    std::fs::write(dir.join(format!("f{f}.py")), "x = 1\n").unwrap();
                }
            }
        }
        for skipped in ["\u{2e}git", "tests"] {
            let dir = root.join(skipped).join("deep");
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..200 {
                std::fs::write(dir.join(format!("o{f}.py")), "y = 2\n").unwrap();
            }
        }

        let full = super::summarize_workdir_to_depth(root, 3);
        assert!(
            full.lines().count() <= super::MAX_WORKDIR_ENTRIES + 1,
            "depth-3 listing must honour MAX_WORKDIR_ENTRIES, got {} lines",
            full.lines().count()
        );
        assert!(
            !full.contains("tests/") && !full.contains(".git/"),
            "skip-list must exclude .git and tests from the listing"
        );

        // Root-only is what the repository-scale caller asks for: cheap
        // orientation, with the rest left to ls/find/grep.
        let root_only = super::summarize_workdir_to_depth(root, 0);
        assert!(
            root_only.lines().count() <= 12,
            "root-only listing should be ~6 dirs, got {} lines",
            root_only.lines().count()
        );
        assert!(root_only.len() < full.len());
    }

    /// A zero anchor budget renders nothing, so a repository-scale
    /// caller pays no source-enumeration tokens at all.
    #[test]
    fn zero_anchor_budget_renders_no_source() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.py"), "def f():\n    return 1\n").unwrap();
        let with = super::render_workdir_anchors_with_promoted(
            tmp.path(),
            &Default::default(),
            crate::runner::DEFAULT_ANCHOR_BUDGET_LINES,
        );
        let without =
            super::render_workdir_anchors_with_promoted(tmp.path(), &Default::default(), 0);
        assert!(
            with.contains("def f()"),
            "default budget should render source"
        );
        assert!(without.is_empty(), "zero budget must render nothing");
    }

    /// Diagnostic probe: measure the two prompt-preamble builders
    /// against a REAL repository checkout, not a synthetic tempdir.
    /// Set `NATIVE_PROBE_DIR=/path/to/checkout` to run it.
    ///
    /// Exists because the 2026-08-18 SWE-bench smoke run sent a
    /// 39,251-token prompt into a 16,000-token window and the split
    /// between the two builders was unknown. Guessing which one
    /// dominates is how you fix the wrong one.
    #[test]
    fn probe_preamble_sizes_on_real_checkout() {
        let Ok(dir) = std::env::var("NATIVE_PROBE_DIR") else {
            eprintln!("skipped: set NATIVE_PROBE_DIR to a real checkout");
            return;
        };
        let p = std::path::Path::new(&dir);
        let state = super::summarize_workdir(p);
        let anchors = super::render_workdir_anchors(p);
        let est = |s: &str| s.len() / 4;
        eprintln!("--- preamble probe: {dir}");
        eprintln!(
            "  summarize_workdir : {:>9} chars  ~{:>7} tok  ({} lines)",
            state.len(),
            est(&state),
            state.lines().count()
        );
        eprintln!(
            "  anchors           : {:>9} chars  ~{:>7} tok  ({} lines)",
            anchors.len(),
            est(&anchors),
            anchors.lines().count()
        );
        eprintln!(
            "  TOTAL preamble    : {:>9} chars  ~{:>7} tok",
            state.len() + anchors.len(),
            est(&state) + est(&anchors)
        );
    }

    use super::*;

    #[test]
    fn summarize_workdir_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let s = summarize_workdir(tmp.path());
        assert!(s.contains("empty"));
    }

    #[test]
    fn snapshot_and_restore_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.py"), "x = 1\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/b.py"), "y = 2\n").unwrap();

        let snap = snapshot_workdir(tmp.path());
        assert!(snap.contains_key(std::path::Path::new("a.py")));
        assert!(snap.contains_key(std::path::Path::new("src/b.py")));

        // Mutate both files.
        std::fs::write(tmp.path().join("a.py"), "x = 999\n").unwrap();
        std::fs::write(tmp.path().join("src/b.py"), "y = 999\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.py")).unwrap(),
            "x = 999\n"
        );

        // Restore.
        restore_workdir(tmp.path(), &snap).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.py")).unwrap(),
            "x = 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("src/b.py")).unwrap(),
            "y = 2\n"
        );
    }

    #[test]
    fn anchors_render_inline_for_small_python_file() {
        // Closes class: "Implementer can't ground-truth file line
        // numbers for patch_file because workdir summary is
        // byte-size only." The line-numbered content sits at
        // position 0 of every Implementer turn — sink-adjacent so
        // attention dilution can't bury it. If a future PR drops
        // this anchor, the 4.1 patch-blind class re-opens.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("mod.py"),
            "def foo():\n    return 1\n\ndef bar():\n    return 2\n",
        )
        .unwrap();
        let s = render_workdir_anchors(tmp.path());
        assert!(s.contains("Current source files"));
        assert!(s.contains("`mod.py` (5 lines)"));
        assert!(s.contains("   1: def foo():"));
        assert!(s.contains("   2:     return 1"));
        assert!(s.contains("   4: def bar():"));
    }

    #[test]
    fn extract_referenced_names_pulls_identifiers_from_pseudocode() {
        let items = vec![
            "tokenize (lines 75-90): Add two-char ops".to_string(),
            "Parser.parse_pow: Recurse parse_pow on RHS".to_string(),
            "evaluate function BinOp case: short-circuit".to_string(),
        ];
        let names = extract_referenced_names(&items);
        assert!(names.contains("tokenize"));
        assert!(names.contains("parse_pow"));
        assert!(names.contains("Parser"));
        assert!(names.contains("evaluate"));
        assert!(names.contains("BinOp"));
    }

    #[test]
    fn decl_name_python() {
        assert_eq!(decl_name("def foo(x):"), Some("foo"));
        assert_eq!(decl_name("    def bar():"), Some("bar"));
        assert_eq!(decl_name("class Foo:"), Some("Foo"));
        assert_eq!(decl_name("async def asyncfoo():"), Some("asyncfoo"));
    }

    #[test]
    fn decl_name_rust() {
        assert_eq!(decl_name("pub fn baz() {}"), Some("baz"));
        assert_eq!(decl_name("fn quux() {}"), Some("quux"));
        assert_eq!(decl_name("pub struct Thing"), Some("Thing"));
    }

    #[test]
    fn anchors_promote_named_declarations_to_inline_body() {
        // Closes class: model patches wider than pseudocode says
        // because it can't see the function body in outline mode.
        // Promotion shows the full body for named declarations so
        // the model can replace the precise range.
        let tmp = tempfile::tempdir().unwrap();
        let mut content = String::new();
        // 600 lines — above MAX_FILE_LINES_INLINE (500) so outline
        // mode triggers and promotion has work to do.
        for i in 0..600 {
            if i == 0 {
                content.push_str("def keep_me():\n");
            } else if i == 1 {
                content.push_str("    return 'preserved body line'\n");
            } else if i == 2 {
                content.push_str("    x = 1  # also in keep_me\n");
            } else if i == 3 {
                content.push_str("def other():\n");
            } else {
                content.push_str("    pass\n");
            }
        }
        std::fs::write(tmp.path().join("big.py"), &content).unwrap();
        let mut promoted = std::collections::HashSet::new();
        promoted.insert("keep_me".to_string());
        let s = render_workdir_anchors_with_promoted(
            tmp.path(),
            &promoted,
            crate::runner::DEFAULT_ANCHOR_BUDGET_LINES,
        );
        // Promoted: full body visible with line numbers.
        assert!(s.contains("def keep_me():"));
        assert!(s.contains("preserved body line"));
        assert!(s.contains("x = 1  # also in keep_me"));
        assert!(s.contains("PROMOTED"));
        // Non-promoted (other and the rest): just signature.
        assert!(s.contains("def other():"));
        assert!(
            !s.contains("\n    pass"),
            "body of other() should NOT be inlined"
        );
    }

    #[test]
    fn anchors_render_outline_for_large_file() {
        // Above the inline cap, render function/class signatures
        // with line numbers AND line extents — the model needs the
        // extent to know which range each function occupies and
        // patch within (not across) declaration boundaries.
        //
        // Fixture is 600 lines (one decl every 10 lines) — above
        // MAX_FILE_LINES_INLINE (500) so outline mode triggers.
        let tmp = tempfile::tempdir().unwrap();
        let mut content = String::new();
        for i in 0..600 {
            if i % 10 == 0 {
                content.push_str(&format!("def fn_{i}():\n"));
            } else {
                content.push_str("    pass\n");
            }
        }
        std::fs::write(tmp.path().join("big.py"), &content).unwrap();
        let s = render_workdir_anchors(tmp.path());
        assert!(s.contains("`big.py` (600 lines)"));
        assert!(s.contains("outline mode"));
        assert!(s.contains("def fn_0()"));
        assert!(s.contains("def fn_590()"));
        // Extent annotation must be present so the model knows each
        // declaration's body range.
        assert!(s.contains("[lines 1-10]")); // fn_0 → fn_10 next
        assert!(s.contains("[lines 11-20]")); // fn_10 → fn_20 next
                                              // Last function (fn_590 at line 591) extends to EOF (600).
        assert!(s.contains("[lines 591-600]"));
        // Body lines (`    pass`) must NOT appear in outline mode.
        assert!(!s.contains(":     pass"));
        // Pipe separator must NOT appear — using `:` to avoid the
        // model interpreting the anchor as unified-diff context
        // (observed 4.2 2026-05-23).
        assert!(!s.contains(" | def "));
    }

    #[test]
    fn anchors_skip_tests_and_build_dirs() {
        // tests/ contents are read-only for the model; build/target
        // dirs are generated output. Neither should pollute the
        // anchor block.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
        std::fs::create_dir_all(tmp.path().join("target/release")).unwrap();
        std::fs::write(tmp.path().join("tests/test_x.py"), "def test_x(): pass\n").unwrap();
        std::fs::write(tmp.path().join("target/release/lib.rs"), "pub fn x() {}\n").unwrap();
        std::fs::write(tmp.path().join("mod.py"), "def real(): pass\n").unwrap();
        let s = render_workdir_anchors(tmp.path());
        assert!(s.contains("mod.py"));
        assert!(!s.contains("test_x"), "tests/ leaked into anchors: {s}");
        assert!(!s.contains("target/release"), "target/ leaked: {s}");
    }

    #[test]
    fn anchors_empty_workdir_returns_empty_string() {
        // FromScratch tier before any write: nothing to anchor.
        let tmp = tempfile::tempdir().unwrap();
        let s = render_workdir_anchors(tmp.path());
        assert!(s.is_empty(), "empty workdir should yield no anchor: {s:?}");
    }

    #[test]
    fn anchors_caps_total_lines_across_many_files() {
        // Cap is load-bearing: a workdir with many small files would
        // otherwise dump every one into every prompt. The
        // cap-exceeded marker should be emitted; later files are
        // dropped.
        //
        // Fixture: 30 files × 50 lines = 1500 lines, above the
        // 1200-line MAX_TOTAL_ANCHOR_LINES cap.
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..30 {
            let body = "x = 1\n".repeat(50);
            std::fs::write(tmp.path().join(format!("f{i:02}.py")), &body).unwrap();
        }
        let s = render_workdir_anchors(tmp.path());
        assert!(
            s.contains("further source files omitted") || s.matches("###").count() <= 25,
            "expected anchor cap to drop later files; got {} fenced blocks",
            s.matches("###").count()
        );
    }

    #[test]
    fn format_initial_prompt_includes_anchor_when_source_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.py"), "def x(): pass\n").unwrap();
        let p = format_initial_prompt(tmp.path(), "do the thing");
        assert!(p.contains("## Workdir state"));
        assert!(p.contains("## Current source files"));
        assert!(p.contains("def x"));
        assert!(p.contains("do the thing"));
    }

    #[test]
    fn summarize_workdir_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn x() {}").unwrap();
        let s = summarize_workdir(tmp.path());
        assert!(s.contains("Cargo.toml"));
        assert!(s.contains("src/"));
        assert!(s.contains("lib.rs"));
    }

    #[test]
    fn workdir_hash_changes_on_write() {
        let tmp = tempfile::tempdir().unwrap();
        let h1 = hash_workdir(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let h2 = hash_workdir(tmp.path());
        assert_ne!(h1, h2);
    }

    #[test]
    fn workdir_hash_stable_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let h1 = hash_workdir(tmp.path());
        let h2 = hash_workdir(tmp.path());
        assert_eq!(h1, h2);
    }

    #[test]
    fn build_role_messages_threads_chat_history_after_system_and_user() {
        // Closes class: "role-aware loop can't act on its own prior
        // tool calls because chat history isn't threaded into the
        // next request." If this test ever softens, the build-loop
        // class re-opens.
        let profile = default_profile_for(Role::Evaluator);
        let dossier = RoleDossier::new();
        let history = vec![
            json!({"role": "assistant", "tool_calls": [{"id": "c1", "function": {"name": "build"}}]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "{\"ok\":false}"}),
        ];
        let msgs = build_role_messages(
            Role::Evaluator,
            &profile,
            &dossier,
            "INITIAL USER MESSAGE",
            &history,
        );
        assert_eq!(msgs.len(), 4, "system + user + assistant + tool_result");
        assert_eq!(msgs[0].get("role").and_then(|v| v.as_str()), Some("system"));
        assert_eq!(msgs[1].get("role").and_then(|v| v.as_str()), Some("user"));
        assert_eq!(
            msgs[2].get("role").and_then(|v| v.as_str()),
            Some("assistant")
        );
        assert_eq!(msgs[3].get("role").and_then(|v| v.as_str()), Some("tool"));
        // History payload preserved verbatim.
        assert_eq!(
            msgs[3].get("content").and_then(|v| v.as_str()),
            Some("{\"ok\":false}")
        );
    }

    #[test]
    fn build_role_messages_empty_history_is_just_system_and_user() {
        let profile = default_profile_for(Role::Implementer);
        let dossier = RoleDossier::new();
        let msgs = build_role_messages(Role::Implementer, &profile, &dossier, "INITIAL", &[]);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn build_role_request_body_threads_resolved_model_per_role() {
        // Closes class: "RoleModelMap is read but doesn't reach the
        // wire." The role-aware loop resolves the model via
        // `RoleModelMap::model_for(role, fallback)` and passes the
        // result here. This test pins that the resolved string lands
        // verbatim in body["model"] — any future refactor that
        // re-derives model from the profile or from a different
        // source softens heterogeneous-model dispatch and trips this.
        let profile = default_profile_for(Role::Implementer);
        let msgs = vec![json!({"role": "user", "content": "do x"})];
        let tools: Vec<Value> = vec![];
        let body =
            build_role_request_body("commonwealth/primary", &msgs, &tools, 512, &profile, None);
        assert_eq!(body["model"], "commonwealth/primary");
    }

    #[test]
    fn role_model_map_resolves_per_role_with_fallback() {
        // Closes class: "operator sets --evaluator-model but
        // Implementer's turn still goes to the override." Pins the
        // resolution semantics used at the call site in
        // run_native_role_aware.
        let mut map = commonwealth_agent_tools::RoleModelMap::new();
        map.set(Role::Evaluator, Some("commonwealth/coder".into()));
        let fallback = "commonwealth/primary";
        assert_eq!(map.model_for(Role::Planner, fallback), fallback);
        assert_eq!(map.model_for(Role::Implementer, fallback), fallback);
        assert_eq!(
            map.model_for(Role::Evaluator, fallback),
            "commonwealth/coder"
        );
    }

    #[test]
    fn filter_descriptors_for_evaluator_terminating_subset_excludes_verifiers() {
        // §B structural invariant: after smoke ok, the `tools` array
        // sent to the daemon MUST NOT contain build or smoke. The
        // OpenAI schema validator drops any model attempt to re-call
        // them. If a future PR softens this — by expanding
        // EVALUATOR_TERMINATING_SUBSET or by bypassing
        // filter_descriptors_for at the gate — the build-loop-after-
        // pass class re-opens silently.
        let adapter = native_adapter::Adapter;
        let restricted = filter_descriptors_for(
            &adapter,
            commonwealth_agent_tools::role::EVALUATOR_TERMINATING_SUBSET,
        );
        let names: Vec<&str> = restricted
            .iter()
            .filter_map(|d| {
                d.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
            })
            .collect();
        assert!(
            !names.contains(&"build"),
            "build must be excluded: {names:?}"
        );
        assert!(
            !names.contains(&"smoke"),
            "smoke must be excluded: {names:?}"
        );
        assert!(
            names.contains(&"agent_done"),
            "agent_done present: {names:?}"
        );
        assert!(
            names.contains(&"handoff_to_implementer"),
            "handoff_to_implementer present: {names:?}"
        );
    }

    #[test]
    fn per_tenure_turn_cap_per_role() {
        // §C invariant: per-role caps are the named ceiling. If a
        // future PR tightens one without updating the comment + the
        // FailureClass::RoleTurnCap description, this test trips.
        assert_eq!(per_tenure_turn_cap(Role::Planner), 3);
        assert_eq!(per_tenure_turn_cap(Role::Implementer), 20);
        assert_eq!(per_tenure_turn_cap(Role::Evaluator), 10);
    }

    #[test]
    fn per_tenure_turn_cap_evaluator_under_planner_implementer() {
        // Cap ordering is load-bearing: Planner is the smallest
        // (one-shot role), Evaluator is moderate (build/smoke/done),
        // Implementer is the largest (multi-file authoring). If a
        // refactor inverts this — e.g. moves Planner > Evaluator —
        // the cap fires on legitimate Implementer authoring before
        // catching genuine Planner pathologies.
        assert!(per_tenure_turn_cap(Role::Planner) < per_tenure_turn_cap(Role::Evaluator));
        assert!(per_tenure_turn_cap(Role::Evaluator) < per_tenure_turn_cap(Role::Implementer));
    }

    #[test]
    fn dossier_drives_evaluator_restriction_via_smoke_just_passed() {
        // Pins the exact wiring at run_native_role_aware's gate:
        // smoke_just_passed() == true → restricted subset applies.
        // smoke_just_passed() == false → default profile applies.
        // Mirroring the runtime conditional ensures future tweaks
        // to the truth table can't bypass the gate without tripping
        // this.
        let adapter = native_adapter::Adapter;
        let profile = default_profile_for(Role::Evaluator);

        // 1. Fresh dossier: full Evaluator subset.
        let dossier = RoleDossier::new();
        assert!(!dossier.smoke_just_passed());
        let tools = if matches!(Role::Evaluator, Role::Evaluator) && dossier.smoke_just_passed() {
            filter_descriptors_for(
                &adapter,
                commonwealth_agent_tools::role::EVALUATOR_TERMINATING_SUBSET,
            )
        } else {
            filter_descriptors(&adapter, &profile)
        };
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|d| {
                d.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
            })
            .collect();
        assert!(names.contains(&"build"));
        assert!(names.contains(&"smoke"));

        // 2. Smoke ok: restricted subset.
        let mut dossier = RoleDossier::new();
        dossier.record_verification(PrimitiveKind::Smoke, true, "all tests passed");
        assert!(dossier.smoke_just_passed());
        let tools = if matches!(Role::Evaluator, Role::Evaluator) && dossier.smoke_just_passed() {
            filter_descriptors_for(
                &adapter,
                commonwealth_agent_tools::role::EVALUATOR_TERMINATING_SUBSET,
            )
        } else {
            filter_descriptors(&adapter, &profile)
        };
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|d| {
                d.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
            })
            .collect();
        assert!(!names.contains(&"build"));
        assert!(!names.contains(&"smoke"));
        assert!(names.contains(&"agent_done"));
    }
}
