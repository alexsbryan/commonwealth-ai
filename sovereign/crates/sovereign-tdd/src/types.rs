// SPDX-License-Identifier: AGPL-3.0-or-later
//! Public types for the unified TDD solver.
//!
//! The collapsed shape (2026-05-24): one [`Trial`] carries
//! everything a solver round needs — the workdir under §7.1 gate,
//! the model id, the user-facing prompt that names intent and
//! move-shape, the test command that defines "done", and a
//! [`Polarity`] that flips the fitness predicate for the rare
//! cases (Red phase) where "improvement" means more failures, not
//! fewer. One [`run_trial`](crate::trial::run_trial) function
//! consumes it.
//!
//! Per-phase types (RedResult / GreenResult / RefactorResult /
//! MultiFileResult / RefactorTarget / MultiFileTarget) collapsed
//! into this single surface on 2026-05-24. The validated loop
//! machinery is unchanged; only the framing collapsed.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::workdir::Workdir;

// ── request envelope ────────────────────────────────────────────────

/// One trial = one full invocation of the solver loop. Built either
/// by hand (power-user path) or via a `tasks::*` convenience
/// wrapper that knows how to translate a high-level intent into a
/// prompt + structural-test pair.
pub struct Trial {
    pub workdir: Workdir,
    pub model: String,
    /// User-facing intent + move-shape guidance. Threaded directly
    /// into each round's user message. The model sees this verbatim.
    pub prompt: String,
    /// Shell command that runs the project's tests. Defines the
    /// fitness signal: every round's score is computed by running
    /// this and counting passes / failures.
    pub test_command: String,
    pub polarity: Polarity,
    pub config: TrialConfig,
    /// Optional pre-write syntax validator. When set, the executor
    /// runs it on the candidate's emitted source before writing —
    /// rejecting malformed code at apply time with cargo-shape
    /// feedback instead of writing it and failing at test time.
    /// Catches the "model wrote unparseable Python/Rust that
    /// pytest/cargo can't even import" failure class observed in
    /// lights-out trial-2 (2026-05-24 N=5 probe). None disables
    /// validation; the bench adapter wires the language-appropriate
    /// validator from `AgentRunContext.syntax_validator`.
    pub syntax_validator: Option<sovereign_agent_tools::syntax::DynSyntaxValidator>,
}

/// Acceptance-predicate polarity. The loop is the same shape under
/// both; only `is_strict_improvement` flips.
#[derive(Debug, Clone)]
pub enum Polarity {
    /// Accept when `passed` strictly increases. Covers everything
    /// that used to be Green / Refactor / MultiFile: structural
    /// goals encoded as tests, bug fixes, anything where "more
    /// tests passing" is the gradient.
    MaximizePassing,
    /// Accept when at least one new test appeared, EVERY new test
    /// fails, and no previously-passing test regressed. The Red
    /// polarity — we want discriminating test(s), not a fix. A
    /// multi-case pin (2-4 focused cases in one file) is accepted;
    /// a candidate whose new tests include a vacuous pass is not.
    GenerateOneFailing {
        /// Optional hint at the test name the model should add.
        /// The loop doesn't enforce a match — just surfaces it in
        /// the prompt for guidance.
        test_name_hint: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct TrialConfig {
    pub candidates_per_round: usize,
    pub rounds_per_trial: usize,
    pub max_stall_rounds: u32,
    pub emit_max_tokens: u32,
    pub candidate_test_timeout: Duration,
    pub temp_ladder_default: Vec<f32>,
    pub temp_ladder_wide: Vec<f32>,
    /// Run each round's candidates one at a time instead of in
    /// parallel. For test commands whose runs can't overlap —
    /// Playwright suites start a webServer on a fixed port, so
    /// parallel candidates would collide on the bind (or silently
    /// share one server and test the wrong tree).
    pub serial_candidates: bool,
}

impl Default for TrialConfig {
    fn default() -> Self {
        // Defaults pinned to the 2026-05-24 Python-prototype-validated
        // values. The solver loop's median 20/20 on the 4.2-mini-
        // evaluator was measured at these settings.
        Self {
            // 6 candidates: the primary is a ~3B-active MoE — decode
            // is cheap, so widen the sample per round (operator
            // philosophy: more faster trials; 2026-07-06 receipts put
            // per-candidate p(compiling Rust fn) ≈ 0.2-0.35, so 6
            // samples/round ≈ 0.77-0.92 round-level hit rate vs
            // 0.59-0.82 at 4).
            candidates_per_round: 6,
            rounds_per_trial: 6,
            max_stall_rounds: 3,
            emit_max_tokens: 4000,
            candidate_test_timeout: Duration::from_secs(60),
            temp_ladder_default: vec![0.2, 0.4, 0.7, 0.9],
            temp_ladder_wide: vec![0.3, 0.6, 0.9, 1.1],
            serial_candidates: false,
        }
    }
}

// ── round observer ──────────────────────────────────────────────────

/// Callback invoked by [`run_trial_observed`](crate::trial::run_trial_observed)
/// once per completed round, with the same [`RoundSummary`] that
/// lands in the trajectory. Lets a job host stream rounds live
/// (SSE, progress UI) without the engine knowing about transports.
/// Must be cheap and non-blocking — it runs inline between rounds.
pub type RoundObserver = std::sync::Arc<dyn Fn(&RoundSummary) + Send + Sync>;

// ── response envelope ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialResult {
    pub status: TrialStatus,
    pub tests_before: TestSummary,
    pub tests_after: TestSummary,
    pub rounds: u32,
    pub trajectory: Vec<RoundSummary>,
    /// Diff produced by the winning trajectory. Empty when no round
    /// improved (Stalled / Exhausted / NoBaseline / Errored).
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TrialStatus {
    /// Hit the polarity's terminal state — `MaximizePassing` got all
    /// tests passing; `GenerateOneFailing` produced the expected
    /// single new failure.
    Reached,
    /// Strict fitness improvement vs baseline, but not at the
    /// terminal state. Caller may want to continue with a new trial.
    Improved,
    /// Loop ran rounds_without_improvement consecutive rounds where
    /// no candidate beat the current fitness. Honest stop.
    Stalled { rounds_without_improvement: u32 },
    /// Round budget exhausted while still making forward progress.
    /// Distinct from `Stalled` so the caller can decide "give it
    /// more rounds" (Exhausted) vs "intervene" (Stalled).
    Exhausted { rounds: u32 },
    /// Couldn't compute a baseline test result (no source file, no
    /// runnable test command). The loop never had a fitness signal.
    NoBaseline { reason: String },
    /// Pre-flight failure (backend down, snapshot failed, scratch
    /// dir error, etc).
    Errored { reason: String },
}

// ── per-round / per-test shared ─────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestSummary {
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
    pub failed_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundSummary {
    pub round: u32,
    /// One label per candidate: `shape@temp=passing`
    /// (e.g. `rewrite-evaluate@T0.4=8`).
    pub candidates: Vec<String>,
    /// Winning candidate's `shape@temp` or `None` on stall.
    pub winner: Option<String>,
    pub passing_after: u32,
    pub failed_after: u32,
    /// Per-candidate receipts: why each candidate landed where it
    /// did. The labels above stay terse for prompt-side history;
    /// this carries the diagnostic detail (error class + message,
    /// emission size, response tail) for post-run analysis.
    #[serde(default)]
    pub details: Vec<CandidateDetail>,
}

/// Diagnostic receipt for one candidate in one round. Persisted in
/// the trajectory so a stalled trial can be diagnosed from artifacts
/// alone — without re-running the trial under a debugger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateDetail {
    /// Edit shape (`rewrite <fn>` / `patch a-b` / `<parse-failed>` …).
    pub shape: String,
    pub temp: f32,
    /// `NNp/MMf` on a run that reached tests, else `err:<class>`
    /// where class ∈ {backend, parse, apply, snapshot}.
    pub outcome: String,
    /// Full error message (capped) when the candidate errored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Char length of the model's emitted source body (0 when the
    /// response never parsed).
    pub body_chars: usize,
    /// Last ~200 chars of the emitted body — enough to spot
    /// truncation, reasoning-leak-into-code, and fence problems.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_tail: Option<String>,
    /// True when the applied edit came from the pointed repair turn
    /// (first apply rejected, one follow-up call fixed it).
    #[serde(default)]
    pub repaired: bool,
}
