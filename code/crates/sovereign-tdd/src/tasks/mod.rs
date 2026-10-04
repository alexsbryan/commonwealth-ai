// SPDX-License-Identifier: AGPL-3.0-or-later
//! Convenience wrappers — preset (prompt, polarity,
//! structural-test) combos. Each task is a pure function that
//! consumes a workdir + intent and returns a [`Trial`] the caller
//! hands to [`crate::trial::run_trial`].
//!
//! Tasks are opt-in. Power users build a `Trial` by hand; common
//! cases use a task wrapper for the prompt + test-generator
//! ergonomics.
//!
//! The architectural claim: most "task types" you'd want to
//! support are 30-line files in this directory. Adding a new task
//! shape never requires touching the core loop.

pub mod bdd;
pub mod framework;
pub mod make_passing;
pub mod solve;
pub mod split_file;
pub mod structural;
pub mod write_failing_test;

pub use bdd::{
    bdd_cycle, bdd_cycle_observed, BddCycleArgs, BddCycleResult, BddRoundObserver, BddStage,
    ReviewMode,
};
pub use framework::{
    detect_framework, has_playwright_config, is_playwright_command, trial_config_for_command,
    Framework,
};
pub use make_passing::make_failing_tests_pass;
pub use solve::{
    solve, SolveArgs, SolveOutcome, SolvePath, SolveRoundObserver, SolveStage, SolveVerb,
};
pub use split_file::split_file;
pub use write_failing_test::write_failing_test;
