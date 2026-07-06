// SPDX-License-Identifier: AGPL-3.0-or-later
//! # commonwealth-tdd
//!
//! Unified TDD solver loop. One [`run_trial`](trial::run_trial)
//! function that takes a [`Trial`](types::Trial) and returns a
//! [`TrialResult`](types::TrialResult). Per-task convenience
//! wrappers live in [`tasks`].
//!
//! The architectural pattern is the **solver loop**: parallel
//! candidate generation at varied temperatures, monotonic
//! improvement gating, fitness function judges, no defensive
//! parsing. Validated on Green-phase (median 20/20 on
//! 4.2-mini-evaluator vs role-loop 0-3/9), Red-phase (92%
//! PASS_AS_RED across N=25), and multi-file refactor (probe
//! 2026-05-24: max line count 97 → 78, tests stayed green).
//!
//! ## Collapsed surface (2026-05-24)
//!
//! Per-phase solvers (RedSolver / GreenSolver / RefactorSolver /
//! MultiFileSolver) collapsed into one [`run_trial`]. The fitness
//! predicate flips with [`Polarity`](types::Polarity):
//!
//! - `MaximizePassing` — accept when `passed` strictly increases.
//!   Covers everything that was Green / Refactor / MultiFile.
//! - `GenerateOneFailing` — accept when exactly one new failing
//!   test appeared. The Red polarity.
//!
//! Per-task convenience wrappers materialize structural goals as
//! tests and supply move-shape guidance in the prompt:
//!
//! - [`tasks::make_failing_tests_pass`] — Green-equivalent default.
//! - [`tasks::write_failing_test`] — Red.
//! - [`tasks::split_file`] — generates a `test_max_file_size`
//!   structural test, then runs the trial.

pub mod backend;
pub mod prompts;
pub mod shared;
pub mod tasks;
pub mod trial;
pub mod types;
pub mod workdir;

pub use backend::{
    BackendError, ChatBackend, ChatResponse, DeterministicChatBackend, ReqwestChatBackend,
};
pub use shared::{EditAction, Language, ParsedResponse, TestParseResult, TestRunResult};
pub use trial::run_trial;
pub use types::{
    CandidateDetail, Polarity, RoundSummary, TestSummary, Trial, TrialConfig, TrialResult,
    TrialStatus,
};
pub use workdir::{DirtyWorkdir, Workdir};
