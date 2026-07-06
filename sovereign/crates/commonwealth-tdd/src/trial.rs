// SPDX-License-Identifier: AGPL-3.0-or-later
//! The unified solver loop. One function — [`run_trial`] —
//! replaces what used to be `GreenSolver`, `RedSolver`,
//! `RefactorSolver`, and `MultiFileSolver`.
//!
//! Architecture: parallel K candidates at varied temperatures,
//! monotonic gating on fitness, stall detection. The fitness
//! predicate flips with [`Polarity`]:
//!
//! - `MaximizePassing` — `after.passed > before.passed`. Default.
//! - `GenerateOneFailing` — exactly one new failure, no
//!   previously-passing regressed. Red.
//!
//! Validated 2026-05-24 against the role-loop runner (median 20/20
//! on 4.2-mini-evaluator vs 0-3/9). Behavior preserved across the
//! 2026-05-24 unification refactor — the inner machinery is the
//! same code; only the framing collapsed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use commonwealth_agent_tools::executor::ExecCtx;
use serde_json::{json, Value};
use tokio::task::JoinSet;

use crate::backend::ChatBackend;
use crate::prompts::TRIAL_SYSTEM_PROMPT;
use crate::shared::{
    apply_edit, discover_source_files, has_dangling_action, parse_response_edits,
    render_with_line_numbers, run_tests,
    snapshot_dir, EditAction, Language, ParsedResponse, TestRunResult,
};
use crate::types::{Polarity, RoundSummary, TestSummary, Trial, TrialResult, TrialStatus};

pub async fn run_trial(trial: Trial, backend: Arc<dyn ChatBackend>) -> TrialResult {
    let _started = Instant::now();
    let base_workdir = trial.workdir.path().to_path_buf();
    let polarity = trial.polarity.clone();
    let config = trial.config.clone();
    let prompt = trial.prompt.clone();
    let model = trial.model.clone();
    let test_command = trial.test_command.clone();
    let syntax_validator = trial.syntax_validator.clone();

    // Scratch dir for per-candidate snapshots. MUST live outside the
    // canonical workdir (otherwise snapshot_dir recurses on itself).
    let scratch_holder = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return errored(&format!("scratch tmpdir: {e}")),
    };
    let scratch_root = scratch_holder.path().to_path_buf();

    // Pristine baseline snapshot — used by the anti-plateau strategy.
    // When the loop stalls (round 0 winner promoted, but subsequent
    // rounds can't beat the partial fit), the search may be trapped
    // in a local optimum where every patch lands NEAR a structurally-
    // wrong solution. ONE candidate per stall round snapshots from
    // this frozen baseline instead of the carried-forward winner so
    // the model can attempt an architectural restart from scratch.
    // Empirically motivated: lights-out trial-0 2026-05-24 stalled
    // for 3 rounds at 2/3 with every candidate producing variants of
    // a buggy GF(2) implementation; restarting from baseline gives
    // the model a clean slate to re-attempt the algorithm.
    let pristine_baseline = scratch_root.join("__pristine_baseline__");
    if let Err(e) = snapshot_dir(&base_workdir, &pristine_baseline) {
        return errored(&format!("pristine baseline snapshot: {e}"));
    }

    // Source file discovery is best-effort. The loop still runs if
    // no source file is found — the model gets a blank `file_listing`
    // and writes new files via WriteFile. Required for the
    // canonical Green-phase shape (where a source file exists and
    // gets patched), optional for Red (which writes test files)
    // and the multi-file path (where the user prompt names targets).
    let source_files = discover_source_files(&base_workdir);
    let source_file = source_files.first().cloned();
    let language = source_file
        .as_deref()
        .map(Language::from_path)
        .unwrap_or(Language::Python);

    let baseline = run_tests(
        &base_workdir,
        &test_command,
        language,
        config.candidate_test_timeout,
    )
    .await;
    let tests_before = TestSummary {
        passed: baseline.parsed.passed,
        failed: baseline.parsed.failed,
        total: baseline.parsed.total,
        failed_names: baseline.parsed.failed_names.clone(),
    };
    tracing::info!(
        passed = tests_before.passed,
        failed = tests_before.failed,
        total = tests_before.total,
        polarity = ?polarity,
        "trial: baseline"
    );

    // Polarity-specific short-circuit checks before the loop.
    if let Polarity::MaximizePassing = polarity {
        // `total > 0 && passed == total` — already at terminal state.
        if tests_before.total > 0 && tests_before.passed == tests_before.total {
            tracing::info!("trial: baseline already passing — Reached");
            return TrialResult {
                status: TrialStatus::Reached,
                tests_before: tests_before.clone(),
                tests_after: tests_before,
                rounds: 0,
                trajectory: vec![],
                diff: String::new(),
            };
        }
        // No tests at all — no baseline fitness signal.
        if tests_before.total == 0 {
            tracing::warn!("trial: no tests discovered — NoBaseline");
            return TrialResult {
                status: TrialStatus::NoBaseline {
                    reason: "test_command produced no test results — \
                             write at least one test or check the verify_cmd"
                        .into(),
                },
                tests_before: tests_before.clone(),
                tests_after: tests_before,
                rounds: 0,
                trajectory: vec![],
                diff: String::new(),
            };
        }
    }

    let mut current = tests_before.clone();
    let mut current_tail = baseline.tail.clone();
    let mut history: Vec<String> = vec![];
    let mut trajectory: Vec<RoundSummary> = vec![];
    let mut rounds_without_improvement: u32 = 0;
    // Consecutive rounds where the candidate pool produced NO
    // outcome different from the base — not even a different failure
    // mix. A dry pool means the sampling distribution is degenerate
    // for this prompt/base; more rounds are the same draw (3.3 H-arm
    // receipts: three rounds of identical 6p/1f ties). Gradient-
    // bearing stalls (varied partial passes) keep the full
    // max_stall_rounds runway.
    let mut dry_rounds: u32 = 0;
    let mut status: Option<TrialStatus> = None;
    let mut winning_body = String::new();
    // Feedback from the previous round's errored candidates,
    // surfaced into the next round's user message. Empty on round 0
    // and after any round where all candidates ran cleanly.
    let mut last_round_feedback: ErrorFeedback = ErrorFeedback::default();

    for round in 0..config.rounds_per_trial {
        // Polarity-aware terminal check.
        if let Polarity::MaximizePassing = polarity {
            if current.total > 0 && current.passed >= current.total {
                status = Some(TrialStatus::Reached);
                break;
            }
        }

        let file_listing = render_source_files(&base_workdir, &source_files);
        let history_block = history
            .iter()
            .rev()
            .take(6)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let stall_state = StallState::from(rounds_without_improvement);
        // The model sees the pristine-baseline file listing on the
        // restart candidate, so the message it generates is grounded
        // in the original code, not the partial-fit winner.
        let pristine_listing = render_source_files(&pristine_baseline, &source_files);
        let feedback_block = last_round_feedback.render(config.candidates_per_round);
        let regular_messages = vec![
            system_message(),
            user_message(
                &prompt,
                &polarity,
                source_file
                    .as_deref()
                    .unwrap_or("(no source file discovered)"),
                &file_listing,
                &current,
                &current_tail,
                &history_block,
                stall_state,
                &feedback_block,
            ),
        ];
        let restart_messages = vec![
            system_message(),
            user_message(
                &prompt,
                &polarity,
                source_file
                    .as_deref()
                    .unwrap_or("(no source file discovered)"),
                &pristine_listing,
                &current,
                &current_tail,
                &history_block,
                StallState::Restart {
                    rounds: rounds_without_improvement,
                },
                &feedback_block,
            ),
        ];

        let temp_ladder: &[f32] = if rounds_without_improvement >= 1 {
            &config.temp_ladder_wide
        } else {
            &config.temp_ladder_default
        };

        let mut join_set: JoinSet<CandidateOutcome> = JoinSet::new();
        for i in 0..config.candidates_per_round {
            let temp = temp_ladder[i % temp_ladder.len()];
            let candidate_workdir = scratch_root.join(format!("r{round}-c{i}"));
            let backend = Arc::clone(&backend);
            let model = model.clone();
            let test_command_local = test_command.clone();
            let source_local = source_files.clone();
            // Restart slot: candidate 0 when the loop has stalled at
            // least one round. Snapshots from pristine_baseline and
            // gets the restart-shaped prompt so the model sees a
            // clean slate and is told to try a different approach.
            // Other candidates continue refining the current winner.
            let is_restart = i == 0 && rounds_without_improvement >= 1;
            let messages = if is_restart {
                restart_messages.clone()
            } else {
                regular_messages.clone()
            };
            let base = if is_restart {
                pristine_baseline.clone()
            } else {
                base_workdir.clone()
            };
            let timeout = config.candidate_test_timeout;
            let max_tokens = config.emit_max_tokens;
            let validator = syntax_validator.clone();
            join_set.spawn(async move {
                try_candidate(
                    backend,
                    candidate_workdir,
                    source_local,
                    test_command_local,
                    language,
                    model,
                    messages,
                    temp,
                    base,
                    validator,
                    timeout,
                    max_tokens,
                )
                .await
            });
        }
        let mut candidates: Vec<CandidateOutcome> = Vec::with_capacity(config.candidates_per_round);
        while let Some(j) = join_set.join_next().await {
            if let Ok(c) = j {
                candidates.push(c);
            }
        }

        let candidate_labels: Vec<String> = candidates
            .iter()
            .map(|c| format!("{}@T{}={}", c.shape_summary(), c.temp, c.passing_or_error()))
            .collect();
        let candidate_details: Vec<crate::types::CandidateDetail> =
            candidates.iter().map(|c| c.detail()).collect();

        // Pick the strict-improvement winner under the active polarity.
        let winner = candidates
            .iter()
            .filter(|c| c.outcome.is_ok())
            .filter(|c| {
                let after = c.summary();
                is_strict_improvement(&current, &after, &polarity)
            })
            // Tie-break: under MaximizePassing prefer most passes;
            // under GenerateOneFailing all valid candidates are
            // equally "exactly one new failure" so we take the first.
            .max_by_key(|c| c.summary().passed);

        if let Some(w) = winner {
            let after = w.summary();
            tracing::info!(
                round,
                before_passed = current.passed,
                after_passed = after.passed,
                after_failed = after.failed,
                shape = %w.shape_summary(),
                temp = w.temp,
                "trial: round improved"
            );
            if let Err(e) = snapshot_dir(&w.workdir, &base_workdir) {
                tracing::warn!(error = %e, "trial: failed to promote winner workdir");
            }
            current = after.clone();
            current_tail = w
                .outcome
                .as_ref()
                .map(|t| t.tail.clone())
                .unwrap_or_default();
            winning_body = w.body.clone();
            history.push(format!(
                "round {round}: {} (T={}) → passing={} failing={}",
                w.shape_summary(),
                w.temp,
                after.passed,
                after.failed
            ));
            rounds_without_improvement = 0;
            dry_rounds = 0;
            trajectory.push(RoundSummary {
                round: round as u32,
                candidates: candidate_labels,
                winner: Some(format!("{}@T{}", w.shape_summary(), w.temp)),
                passing_after: current.passed,
                failed_after: current.failed,
                details: candidate_details,
            });
            // Polarity-aware terminal check after winning.
            if reached_terminal(&current, &polarity) {
                status = Some(TrialStatus::Reached);
                break;
            }
        } else {
            rounds_without_improvement = rounds_without_improvement.saturating_add(1);
            tracing::info!(
                round,
                current_passed = current.passed,
                rounds_without_improvement,
                "trial: no improvement"
            );
            history.push(format!(
                "round {round}: no improvement [{}]",
                candidate_labels.join(", ")
            ));
            trajectory.push(RoundSummary {
                round: round as u32,
                candidates: candidate_labels,
                winner: None,
                passing_after: current.passed,
                failed_after: current.failed,
                details: candidate_details,
            });
            let round_has_novelty = candidates.iter().any(|c| match &c.outcome {
                Ok(t) => {
                    t.parsed.total > 0
                        && (t.parsed.passed, t.parsed.failed) != (current.passed, current.failed)
                }
                Err(_) => false,
            });
            if round_has_novelty {
                dry_rounds = 0;
            } else {
                dry_rounds = dry_rounds.saturating_add(1);
            }
            if dry_rounds >= 2 {
                tracing::info!(round, "trial: two consecutive dry rounds — early stall");
                status = Some(TrialStatus::Stalled {
                    rounds_without_improvement,
                });
                break;
            }
            if rounds_without_improvement >= config.max_stall_rounds {
                status = Some(TrialStatus::Stalled {
                    rounds_without_improvement,
                });
                break;
            }
        }
        // Prepare feedback for the next round: bucket this round's
        // errored candidates so the model can see specifically what
        // went wrong, with cargo-shape "error / reason / help" text.
        // On clean rounds this is empty (no errored candidates → no
        // feedback block). On stall rounds it gives the model
        // actionable signal to avoid the same shape of failure.
        last_round_feedback = ErrorFeedback::from_candidates(&candidates);
    }
    // rounds_completed = every round that pushed a trajectory entry,
    // INCLUDING the one we broke out on (Reached / Stalled both push
    // before the break).
    let rounds_completed = trajectory.len() as u32;

    let final_status = status.unwrap_or_else(|| {
        if reached_terminal(&current, &polarity) {
            TrialStatus::Reached
        } else if has_progressed(&tests_before, &current, &polarity) {
            TrialStatus::Improved
        } else {
            TrialStatus::Exhausted {
                rounds: rounds_completed,
            }
        }
    });

    drop(scratch_holder);

    TrialResult {
        status: final_status,
        tests_before,
        tests_after: current,
        rounds: rounds_completed,
        trajectory,
        diff: winning_body,
    }
}

// ── polarity-aware predicates ───────────────────────────────────────

fn is_strict_improvement(before: &TestSummary, after: &TestSummary, polarity: &Polarity) -> bool {
    match polarity {
        Polarity::MaximizePassing => after.passed > before.passed,
        Polarity::GenerateOneFailing { .. } => {
            after.failed == before.failed.saturating_add(1)
                && after.passed == before.passed
                && after.total == before.total.saturating_add(1)
        }
    }
}

fn reached_terminal(current: &TestSummary, polarity: &Polarity) -> bool {
    match polarity {
        Polarity::MaximizePassing => current.total > 0 && current.passed >= current.total,
        // For GenerateOneFailing, the FIRST strict-improvement
        // already IS the terminal state — we wanted exactly one new
        // failure and we got it.
        Polarity::GenerateOneFailing { .. } => current.failed > 0,
    }
}

fn has_progressed(before: &TestSummary, after: &TestSummary, polarity: &Polarity) -> bool {
    match polarity {
        Polarity::MaximizePassing => after.passed > before.passed,
        Polarity::GenerateOneFailing { .. } => after.failed > before.failed,
    }
}

fn errored(reason: &str) -> TrialResult {
    TrialResult {
        status: TrialStatus::Errored {
            reason: reason.to_string(),
        },
        tests_before: TestSummary::default(),
        tests_after: TestSummary::default(),
        rounds: 0,
        trajectory: vec![],
        diff: String::new(),
    }
}

// ── candidate worker ────────────────────────────────────────────────

#[derive(Clone)]
struct CandidateOutcome {
    temp: f32,
    /// Applied edits, in order. Empty = the response never parsed.
    /// Usually one; multi-edit TRANSACTIONS carry several (split /
    /// extract goals need coordinated writes — no single edit can
    /// strictly improve the fitness signal).
    edits: Vec<ParsedResponse>,
    workdir: PathBuf,
    outcome: Result<TestRunResult, String>,
    body: String,
    /// True when the pointed repair turn produced the applied edit.
    repaired: bool,
}

impl CandidateOutcome {
    fn passing_or_error(&self) -> String {
        match &self.outcome {
            Ok(t) => format!("{}p/{}f", t.parsed.passed, t.parsed.failed),
            // Keep the label terse but carry the failure CLASS —
            // "err" alone made a stalled trial undiagnosable from
            // its trajectory (all-err rounds, 2026-07-06).
            Err(e) => format!("err:{}", e.split(':').next().unwrap_or("unknown")),
        }
    }

    fn detail(&self) -> crate::types::CandidateDetail {
        const ERROR_CAP: usize = 600;
        const TAIL_CHARS: usize = 200;
        let error = match &self.outcome {
            Ok(_) => None,
            Err(e) => Some(if e.len() > ERROR_CAP {
                let cut = e
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= ERROR_CAP)
                    .last()
                    .unwrap_or(0);
                format!("{}…", &e[..cut])
            } else {
                e.clone()
            }),
        };
        let body_tail = if self.body.is_empty() {
            None
        } else {
            let start = self
                .body
                .char_indices()
                .rev()
                .map(|(i, _)| i)
                .nth(TAIL_CHARS.saturating_sub(1))
                .unwrap_or(0);
            Some(self.body[start..].to_string())
        };
        crate::types::CandidateDetail {
            shape: self.shape_summary(),
            temp: self.temp,
            outcome: self.passing_or_error(),
            error,
            body_chars: self.body.chars().count(),
            body_tail,
            repaired: self.repaired,
        }
    }

    fn summary(&self) -> TestSummary {
        match &self.outcome {
            Ok(t) => TestSummary {
                passed: t.parsed.passed,
                failed: t.parsed.failed,
                total: t.parsed.total,
                failed_names: t.parsed.failed_names.clone(),
            },
            Err(_) => TestSummary::default(),
        }
    }

    fn shape_summary(&self) -> String {
        fn one(r: &ParsedResponse) -> String {
            let shape = match &r.action {
                EditAction::RewriteFunction { name } => format!("rewrite {name}"),
                EditAction::PatchLines { start, end } => format!("patch {start}-{end}"),
                EditAction::InsertBefore { line } => format!("insert@{line}"),
                EditAction::WriteFile { path } => match path {
                    Some(p) => format!("write_file→{p}"),
                    None => "write_file".to_string(),
                },
            };
            // `~` marks a header-inferred action (model emitted only
            // the source block) — keeps inference-rescued candidates
            // attributable in the trajectory.
            if r.inferred {
                format!("~{shape}")
            } else {
                shape
            }
        }
        let shape = match self.edits.len() {
            0 => return "<parse-failed>".to_string(),
            1 => one(&self.edits[0]),
            _ => format!(
                "txn[{}]",
                self.edits.iter().map(one).collect::<Vec<_>>().join("; ")
            ),
        };
        if self.repaired {
            format!("{shape}+r")
        } else {
            shape
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn try_candidate(
    backend: Arc<dyn ChatBackend>,
    candidate_workdir: PathBuf,
    source_files: Vec<String>,
    test_command: String,
    language: Language,
    model: String,
    messages: Vec<Value>,
    temperature: f32,
    base_workdir: PathBuf,
    syntax_validator: Option<commonwealth_agent_tools::syntax::DynSyntaxValidator>,
    timeout: std::time::Duration,
    emit_max_tokens: u32,
) -> CandidateOutcome {
    if let Err(e) = snapshot_dir(&base_workdir, &candidate_workdir) {
        return CandidateOutcome {
            temp: temperature,
            edits: vec![],
            workdir: candidate_workdir,
            outcome: Err(format!("snapshot: {e}")),
            body: String::new(),
            repaired: false,
        };
    }
    let resp = match backend
        .complete(&model, messages.clone(), temperature, emit_max_tokens)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return CandidateOutcome {
                temp: temperature,
                edits: vec![],
                workdir: candidate_workdir,
                outcome: Err(format!("backend: {e}")),
                body: String::new(),
                repaired: false,
            };
        }
    };
    let mut content = resp.content;
    // Spontaneous-EOS completion: the model declared an action fence
    // and stopped before its source block (MTP emits finish=Stop
    // mid-response; detect by CONTENT — chaos-side pattern). ONE
    // continuation call, then parse the combined text so dangling
    // actions pair with their late-arriving blocks.
    if has_dangling_action(&content) {
        let cont_msgs = {
            let mut m = messages.clone();
            m.push(json!({ "role": "assistant", "content": content }));
            m.push(json!({ "role": "user", "content": "Continue exactly where you stopped: emit the fenced source block for the action you declared, plus any remaining action+block pairs from your plan. No re-planning, no commentary." }));
            m
        };
        if let Ok(cont) = backend
            .complete(&model, cont_msgs, temperature, emit_max_tokens)
            .await
        {
            content.push('\n');
            content.push_str(&cont.content);
        }
    }
    let edits = parse_response_edits(&content);
    if edits.is_empty() {
        return CandidateOutcome {
            temp: temperature,
            edits: vec![],
            workdir: candidate_workdir,
            outcome: Err("parse: no action+block found".into()),
            // Keep the raw content: a parse failure is only
            // diagnosable from what the model actually said.
            body: content,
            repaired: false,
        };
    }
    // Default edit target = first discovered source file. For
    // rewrite_function the target is resolved ACROSS the discovered
    // files — the model names the function, the harness locates its
    // home file (multi-file packages: forcing every edit into the
    // first file made cross-file fixes structurally impossible —
    // 5.1-minilang B-arm 2026-07-06). Edits apply in order as ONE
    // transaction; the first failure aborts (workdir is a snapshot,
    // so partial application is discarded with the candidate).
    let default_target = source_files
        .first()
        .cloned()
        .unwrap_or_else(|| "_unspecified.py".to_string());
    let mut ctx = ExecCtx::new(candidate_workdir.clone());
    if let Some(v) = syntax_validator {
        ctx = ctx.with_syntax_validator(v);
    }
    async fn apply_all(
        ctx: &ExecCtx,
        workdir: &std::path::Path,
        source_files: &[String],
        default_target: &str,
        edits: &[ParsedResponse],
    ) -> Result<(), String> {
        for (i, e) in edits.iter().enumerate() {
            let target = resolve_edit_target(workdir, source_files, &e.action)
                .unwrap_or_else(|| default_target.to_string());
            if let Err(err) = apply_edit(ctx, &target, e).await {
                // Full multi-line cargo-shape error message preserved
                // so the repair turn / next round's prompt can surface
                // it — the model can't fix what it can't see.
                let which = if edits.len() > 1 {
                    format!(" (edit {} of {})", i + 1, edits.len())
                } else {
                    String::new()
                };
                return Err(format!("apply{which}: {}", err.render_for_agent()));
            }
        }
        Ok(())
    }
    let mut edits = edits;
    let mut repaired = false;
    if let Err(first_err) = apply_all(
        &ctx,
        &candidate_workdir,
        &source_files,
        &default_target,
        &edits,
    )
    .await
    {
        // Pointed repair turn — ONE follow-up call on a rejected
        // apply. The harness holds a rendered, line-anchored error
        // for a candidate it is about to discard; small models fix
        // pointed errors far better than they avoid them cold
        // (B-arm 3.2-lights-out t1: 7/12 candidates died to the
        // pre-write syntax check). One repair per candidate keeps
        // the call budget bounded at 2x worst-case.
        let repair_msgs = {
            let mut m = messages.clone();
            m.push(json!({ "role": "assistant", "content": content.clone() }));
            m.push(json!({ "role": "user", "content": format!(
                "The harness rejected that edit:\n\n```\n{first_err}\n```\n\nFix ONLY the reported error — keep the same action(s) and keep every other line of your source block(s) identical. Re-emit the full response (each fenced JSON action followed by its fenced source block). Smallest possible change; no commentary."
            ) }));
            m
        };
        let mut repair_ok = false;
        if let Ok(r2) = backend
            .complete(&model, repair_msgs, temperature, emit_max_tokens)
            .await
        {
            let edits2 = parse_response_edits(&r2.content);
            if !edits2.is_empty() {
                match apply_all(
                    &ctx,
                    &candidate_workdir,
                    &source_files,
                    &default_target,
                    &edits2,
                )
                .await
                {
                    Ok(_) => {
                        edits = edits2;
                        repaired = true;
                        repair_ok = true;
                    }
                    Err(e2) => {
                        let body2 = edits2
                            .iter()
                            .map(|e| e.body.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        return CandidateOutcome {
                            temp: temperature,
                            edits: edits2,
                            workdir: candidate_workdir,
                            outcome: Err(format!("{e2} [after repair; first: {first_err}]")),
                            body: body2,
                            repaired: true,
                        };
                    }
                }
            }
        }
        if !repair_ok {
            let body = edits
                .iter()
                .map(|e| e.body.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            return CandidateOutcome {
                temp: temperature,
                edits,
                workdir: candidate_workdir,
                outcome: Err(first_err),
                body,
                repaired: false,
            };
        }
    }
    let test_result = run_tests(&candidate_workdir, &test_command, language, timeout).await;
    let body = edits
        .iter()
        .map(|e| e.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    CandidateOutcome {
        temp: temperature,
        edits,
        workdir: candidate_workdir,
        outcome: Ok(test_result),
        body,
        repaired,
    }
}

/// Render every discovered source file, path-labeled and
/// line-numbered, under a shared character budget. Files past the
/// budget are listed by name so the model knows they exist (silent
/// truncation would read as "covered everything").
fn render_source_files(root: &std::path::Path, files: &[String]) -> String {
    const BUDGET_CHARS: usize = 14_000;
    let mut out = String::new();
    let mut omitted: Vec<&str> = Vec::new();
    for f in files {
        let rendered = render_with_line_numbers(&root.join(f));
        if out.len() + rendered.len() > BUDGET_CHARS && !out.is_empty() {
            omitted.push(f);
            continue;
        }
        out.push_str(&format!("### `{f}`\n{rendered}\n\n"));
    }
    if !omitted.is_empty() {
        out.push_str(&format!(
            "(additional files not shown, address by path via write_file: {})\n",
            omitted.join(", ")
        ));
    }
    out
}

/// Locate the file an edit action should land in. `rewrite_function`
/// searches the discovered files for the named function (first hit
/// wins, shallowest-first order); `write_file` honors an explicit
/// path. Everything else (patches, pathless writes) → None, meaning
/// the caller's default target.
fn resolve_edit_target(
    workdir: &std::path::Path,
    source_files: &[String],
    action: &EditAction,
) -> Option<String> {
    match action {
        EditAction::RewriteFunction { name } => {
            for f in source_files {
                if let Ok(content) = std::fs::read_to_string(workdir.join(f)) {
                    if commonwealth_agent_tools::executor::find_function_bounds(&content, name)
                        .is_some()
                    {
                        return Some(f.clone());
                    }
                }
            }
            None
        }
        EditAction::WriteFile { path: Some(p) } => Some(p.clone()),
        _ => None,
    }
}

// ── prompts ─────────────────────────────────────────────────────────

fn system_message() -> Value {
    json!({ "role": "system", "content": TRIAL_SYSTEM_PROMPT })
}

#[allow(clippy::too_many_arguments)]
fn user_message(
    user_prompt: &str,
    polarity: &Polarity,
    source_file: &str,
    file_listing: &str,
    current: &TestSummary,
    test_tail: &str,
    history: &str,
    stall_state: StallState,
    feedback_block: &str,
) -> Value {
    let polarity_block = render_polarity_block(polarity, current);
    let history_block = if history.is_empty() {
        "(none — this is the first round)".to_string()
    } else {
        history.to_string()
    };
    let actions = render_suggested_actions(polarity);
    let stall_prefix = match stall_state {
        StallState::Fresh => String::new(),
        StallState::Plateau { rounds } => format!(
            "## Plateau warning\n\n{rounds} round(s) without improvement at {passed}/{total}. The patches we've tried aren't breaking through. Consider whether the UNDERLYING APPROACH needs to change — not just the details. An architectural rethink may be required.\n\n",
            passed = current.passed,
            total = current.total,
        ),
        StallState::Restart { rounds } => format!(
            "## Restart slot\n\nYou are looking at the PRISTINE BASELINE — the original code, NOT the partial fit the other candidates are refining. {rounds} round(s) of patching the partial fit hasn't broken through {passed}/{total}, so this candidate is starting over. Propose a completely different overall approach — different algorithm, different data structures, whatever it takes to solve the problem from scratch.\n\n",
            passed = current.passed,
            total = current.total,
        ),
    };
    let content = format!(
        "{stall_prefix}{feedback_block}## Goal\n\n{user_prompt}\n\n## Fitness\n\n{polarity_block}\n\n## Source files (line-numbered; primary: `{source_file}`)\n\n{file_listing}\n\n## Last test output\n\n```\n{test_tail}\n```\n\n## Attempts so far (for diversity — don't repeat)\n\n{history_block}\n\n## Your output\n\nEmit one fenced JSON action describing your edit, then one fenced source code block with the new content. When the goal REQUIRES coordinated changes to several files (e.g. splitting a module), emit multiple action+block pairs in one response — they apply together as a single transaction.\n\n{actions}\n\nPick whichever edit shape best addresses the most-impactful failing test. `rewrite_function` finds the named function in whichever listed file holds it; to create or fully replace a specific file, use {{\"action\": \"write_file\", \"path\": \"<file>\"}}. Line-number actions (patch_lines / insert_before) address the PRIMARY file only. Indent the source block to match the file's existing indent at the edit site. You may plan briefly in plain text before the blocks; only the fenced blocks are parsed, and code blocks must contain code only.\n"
    );
    json!({ "role": "user", "content": content })
}

/// Bucketed last-round error feedback. The loop collects the
/// errored candidates from the previous round, classifies each by
/// failure class, and surfaces one representative sample per class
/// in the next round's prompt. Without this, the model rejects-but-
/// learns-nothing — lights-out trial-2 (2026-05-24 N=3 probe with
/// validator) showed a single trial generating uniformly-broken
/// emissions for 3 rounds because the model had no idea WHY each
/// attempt was rejected, only that they were.
#[derive(Debug, Clone, Default)]
struct ErrorFeedback {
    buckets: Vec<ErrorBucket>,
}

#[derive(Debug, Clone)]
struct ErrorBucket {
    /// Short label: "parse", "apply", "backend", "snapshot".
    class: &'static str,
    /// Number of candidates in the last round that hit this class.
    count: usize,
    /// Most representative error message — full text from
    /// `render_for_agent` (cargo-shape "error / reason / help") so
    /// the model sees actionable suggestions, not just a label.
    sample: String,
}

impl ErrorFeedback {
    /// Build feedback from the previous round's candidate outcomes.
    /// Returns an empty `ErrorFeedback` if no candidate errored.
    fn from_candidates(candidates: &[CandidateOutcome]) -> Self {
        use std::collections::BTreeMap;
        let mut by_class: BTreeMap<&'static str, (usize, Option<String>)> = BTreeMap::new();
        for c in candidates {
            let Err(ref msg) = c.outcome else { continue };
            let class = classify_error(msg);
            let entry = by_class.entry(class).or_insert((0, None));
            entry.0 += 1;
            // Keep the first non-empty sample. Later candidates'
            // errors are usually variants of the same shape so the
            // first is representative.
            if entry.1.is_none() && !msg.trim().is_empty() {
                entry.1 = Some(msg.clone());
            }
        }
        let buckets = by_class
            .into_iter()
            .map(|(class, (count, sample))| ErrorBucket {
                class,
                count,
                sample: sample.unwrap_or_else(|| String::from("(no detail)")),
            })
            .collect();
        Self { buckets }
    }

    fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    fn render(&self, total_candidates: usize) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut s = String::from("## What failed last round\n\n");
        for b in &self.buckets {
            s.push_str(&format!(
                "- `{}` failed {} of {} candidate(s). Sample:\n  ```\n  {}\n  ```\n",
                b.class,
                b.count,
                total_candidates,
                b.sample.replace('\n', "\n  ").trim_end()
            ));
        }
        s.push_str(
            "\nRead the error text carefully — re-emitting the same shape will hit the same rejection. Fix what the error names.\n\n",
        );
        s
    }
}

fn classify_error(msg: &str) -> &'static str {
    if msg.starts_with("parse:") {
        "parse"
    } else if msg.starts_with("apply:") {
        // Distinguish syntax-validator rejections (which start with
        // a specific marker in render_for_agent) from other apply
        // errors when we can. For now bucket together — they're
        // both "your edit was rejected at apply time."
        "apply"
    } else if msg.starts_with("backend:") {
        "backend"
    } else if msg.starts_with("snapshot:") {
        "snapshot"
    } else {
        "other"
    }
}

/// State of the search-loop's stall counter at message-render time.
/// Drives the anti-plateau prompt prefix.
#[derive(Debug, Clone, Copy)]
enum StallState {
    /// Round 0, or the last round improved. Normal prompt.
    Fresh,
    /// One or more rounds with no improvement. Inject a "the
    /// patches aren't breaking through, consider an architectural
    /// change" warning into the user message.
    Plateau { rounds: u32 },
    /// This candidate's workdir was rolled back to the pristine
    /// baseline. The model is told it's starting from scratch and
    /// should propose a different overall approach, not patches.
    Restart { rounds: u32 },
}

impl From<u32> for StallState {
    fn from(rounds: u32) -> Self {
        if rounds == 0 {
            StallState::Fresh
        } else {
            StallState::Plateau { rounds }
        }
    }
}

/// Suggested-action list rendered into the user message.
///
/// Pre-collapse the Green prompt listed only the three surgical
/// actions (`rewrite_function` / `patch_lines` / `insert_before`).
/// Adding `write_file` to the default list regressed lights-out
/// from 8/12 → 1/12 (2026-05-24 parity probe) because the model
/// started rewriting whole files and producing malformed Python.
/// write_file is still a valid emission — Red and split_file
/// surface it via their per-task prompt prefixes — but the default
/// list mirrors the validated Green prompt.
fn render_suggested_actions(polarity: &Polarity) -> &'static str {
    let _ = polarity; // reserved — polarity-aware action sets land next.
    "```json\n{\"action\": \"rewrite_function\", \"name\": \"<name>\"}\n```\n```json\n{\"action\": \"patch_lines\", \"start\": <int>, \"end\": <int>}\n```\n```json\n{\"action\": \"insert_before\", \"line\": <int>}\n```"
}

fn render_polarity_block(polarity: &Polarity, current: &TestSummary) -> String {
    let failing_names = if current.failed_names.is_empty() {
        "  (none)".to_string()
    } else {
        current
            .failed_names
            .iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match polarity {
        Polarity::MaximizePassing => format!(
            "Maximize tests passing. Currently {passed}/{total} ({failed} failing). Your edit lands ONLY if a currently-failing test passes after it — an edit that leaves the counts unchanged is discarded and the next round starts from the same base, so partial steps that don't flip a failing test are wasted. Make the complete change in this response.\n\nFailing tests:\n{failing_names}",
            passed = current.passed,
            failed = current.failed,
            total = current.total,
        ),
        Polarity::GenerateOneFailing { test_name_hint } => {
            let hint = test_name_hint
                .as_deref()
                .map(|n| format!(" Hint: name the test `{n}` if you can.\n"))
                .unwrap_or_default();
            format!(
                "Generate ONE failing test. Currently {passed}/{total}. Your edit must add exactly one new failing test (an assertion-style failure on the unchanged code) without breaking any currently-passing test.{hint}",
                passed = current.passed,
                total = current.total,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(passed: u32, failed: u32, total: u32) -> TestSummary {
        TestSummary {
            passed,
            failed,
            total,
            failed_names: vec![],
        }
    }

    #[test]
    fn maximize_passing_strict_when_passed_strictly_increases() {
        let before = summary(5, 5, 10);
        let same = summary(5, 5, 10);
        let more = summary(6, 4, 10);
        assert!(!is_strict_improvement(
            &before,
            &same,
            &Polarity::MaximizePassing
        ));
        assert!(is_strict_improvement(
            &before,
            &more,
            &Polarity::MaximizePassing
        ));
    }

    #[test]
    fn generate_one_failing_requires_exactly_one_new_failure() {
        let p = Polarity::GenerateOneFailing {
            test_name_hint: None,
        };
        let before = summary(3, 0, 3);
        // Exactly one new failure, no passes lost.
        let win = summary(3, 1, 4);
        assert!(is_strict_improvement(&before, &win, &p));
        // Two new failures — too many. Reject.
        let too_many = summary(3, 2, 5);
        assert!(!is_strict_improvement(&before, &too_many, &p));
        // One new failure but a passing test regressed.
        let regressed = summary(2, 2, 4);
        assert!(!is_strict_improvement(&before, &regressed, &p));
    }

    #[test]
    fn reached_terminal_maximize_when_all_passing() {
        assert!(reached_terminal(
            &summary(5, 0, 5),
            &Polarity::MaximizePassing
        ));
        assert!(!reached_terminal(
            &summary(4, 1, 5),
            &Polarity::MaximizePassing
        ));
        // Zero-test case is NOT terminal for MaximizePassing — that's NoBaseline.
        assert!(!reached_terminal(
            &summary(0, 0, 0),
            &Polarity::MaximizePassing
        ));
    }

    #[test]
    fn reached_terminal_generate_one_failing_when_any_failure_exists() {
        let p = Polarity::GenerateOneFailing {
            test_name_hint: None,
        };
        assert!(reached_terminal(&summary(3, 1, 4), &p));
        assert!(!reached_terminal(&summary(3, 0, 3), &p));
    }

    #[test]
    fn stall_state_from_u32_matches_round_count() {
        assert!(matches!(StallState::from(0), StallState::Fresh));
        assert!(matches!(
            StallState::from(1),
            StallState::Plateau { rounds: 1 }
        ));
        assert!(matches!(
            StallState::from(5),
            StallState::Plateau { rounds: 5 }
        ));
    }

    #[test]
    fn user_message_fresh_has_no_stall_prefix() {
        let v = user_message(
            "p",
            &Polarity::MaximizePassing,
            "f.py",
            "1: x\n",
            &summary(1, 1, 2),
            "tail",
            "h",
            StallState::Fresh,
            "",
        );
        let content = v.get("content").and_then(|c| c.as_str()).unwrap();
        assert!(!content.contains("Plateau warning"));
        assert!(!content.contains("Restart slot"));
    }

    #[test]
    fn user_message_plateau_injects_warning_with_round_count() {
        let v = user_message(
            "p",
            &Polarity::MaximizePassing,
            "f.py",
            "1: x\n",
            &summary(2, 1, 3),
            "tail",
            "h",
            StallState::Plateau { rounds: 2 },
            "",
        );
        let content = v.get("content").and_then(|c| c.as_str()).unwrap();
        assert!(content.contains("Plateau warning"));
        assert!(content.contains("2 round(s)"));
        assert!(content.contains("UNDERLYING APPROACH"));
        // The plateau prefix must come BEFORE the goal block so the
        // model reads it before sinking into the same patch frame.
        let plateau_idx = content.find("Plateau warning").unwrap();
        let goal_idx = content.find("## Goal").unwrap();
        assert!(plateau_idx < goal_idx);
    }

    fn errored(class_prefix: &str, msg: &str) -> CandidateOutcome {
        CandidateOutcome {
            temp: 0.0,
            edits: vec![],
            workdir: std::path::PathBuf::new(),
            outcome: Err(format!("{class_prefix}: {msg}")),
            body: String::new(),
            repaired: false,
        }
    }

    #[test]
    fn error_feedback_groups_candidates_by_class_with_counts() {
        let cands = vec![
            errored("parse", "no action+block found"),
            errored("parse", "no action+block found"),
            errored("apply", "syntax error at line 3"),
        ];
        let fb = ErrorFeedback::from_candidates(&cands);
        assert_eq!(fb.buckets.len(), 2);
        // BTreeMap orders alphabetically — apply, parse
        assert_eq!(fb.buckets[0].class, "apply");
        assert_eq!(fb.buckets[0].count, 1);
        assert_eq!(fb.buckets[1].class, "parse");
        assert_eq!(fb.buckets[1].count, 2);
    }

    #[test]
    fn error_feedback_is_empty_when_no_candidate_errored() {
        let fb = ErrorFeedback::from_candidates(&[]);
        assert!(fb.is_empty());
        assert_eq!(fb.render(4), String::new());
    }

    #[test]
    fn error_feedback_render_surfaces_sample_and_count() {
        let cands = vec![errored("parse", "no action+block found")];
        let fb = ErrorFeedback::from_candidates(&cands);
        let r = fb.render(4);
        assert!(r.contains("What failed last round"));
        assert!(r.contains("`parse` failed 1 of 4"));
        assert!(r.contains("no action+block found"));
        assert!(r.contains("Read the error text carefully"));
    }

    #[test]
    fn user_message_includes_feedback_block_when_supplied() {
        let v = user_message(
            "p", &Polarity::MaximizePassing, "f.py", "1: x\n",
            &summary(2, 1, 3), "tail", "h",
            StallState::Plateau { rounds: 1 },
            "## What failed last round\n\n- `parse` failed 2 of 4 candidate(s). Sample:\n  ```\n  no action+block found\n  ```\n",
        );
        let content = v.get("content").and_then(|c| c.as_str()).unwrap();
        assert!(content.contains("What failed last round"));
        assert!(content.contains("no action+block found"));
        // Feedback must come BEFORE the goal block so the model
        // reads it before sinking into its next attempt.
        let feedback_idx = content.find("What failed last round").unwrap();
        let goal_idx = content.find("## Goal").unwrap();
        assert!(feedback_idx < goal_idx);
    }

    #[test]
    fn user_message_restart_tells_model_clean_slate() {
        let v = user_message(
            "p",
            &Polarity::MaximizePassing,
            "f.py",
            "1: x\n",
            &summary(2, 1, 3),
            "tail",
            "h",
            StallState::Restart { rounds: 3 },
            "",
        );
        let content = v.get("content").and_then(|c| c.as_str()).unwrap();
        assert!(content.contains("Restart slot"));
        assert!(content.contains("PRISTINE BASELINE"));
        assert!(content.contains("different overall approach"));
    }
}

#[cfg(test)]
mod multi_file_target_tests {
    use super::*;

    #[test]
    fn rewrite_resolves_to_the_file_holding_the_function() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("minilang")).unwrap();
        std::fs::write(tmp.path().join("minilang/__init__.py"), "from .evaluator import evaluate_ast\n").unwrap();
        std::fs::write(
            tmp.path().join("minilang/evaluator.py"),
            "def evaluate_ast(node, env):\n    return None\n",
        )
        .unwrap();
        let files = vec![
            "minilang/__init__.py".to_string(),
            "minilang/evaluator.py".to_string(),
        ];
        let action = EditAction::RewriteFunction {
            name: "evaluate_ast".into(),
        };
        assert_eq!(
            resolve_edit_target(tmp.path(), &files, &action).as_deref(),
            Some("minilang/evaluator.py")
        );
    }

    #[test]
    fn explicit_write_file_path_wins_and_patches_stay_default() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec!["a.py".to_string()];
        let w = EditAction::WriteFile {
            path: Some("pkg/new_module.py".into()),
        };
        assert_eq!(
            resolve_edit_target(tmp.path(), &files, &w).as_deref(),
            Some("pkg/new_module.py")
        );
        let p = EditAction::PatchLines { start: 1, end: 3 };
        assert!(resolve_edit_target(tmp.path(), &files, &p).is_none());
    }

    #[test]
    fn render_source_files_labels_paths_and_reports_omissions() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.py"), "x = 1\n").unwrap();
        std::fs::write(tmp.path().join("b.py"), "y = 2\n").unwrap();
        let out = render_source_files(tmp.path(), &["a.py".into(), "b.py".into()]);
        assert!(out.contains("### `a.py`"));
        assert!(out.contains("### `b.py`"));
        assert!(!out.contains("additional files not shown"));
    }
}
