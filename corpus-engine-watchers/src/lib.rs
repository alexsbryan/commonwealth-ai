// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background reactive layer for the corpus/agent workspace: the
//! lint & test watchers, the project-index watcher, their result
//! stores, and the coordinator that supervises them.
//!
//! Carved out of `corpus-engine` (R4 Step 1, see
//! `corpus-engine/DECOMPOSITION.md`). The point of the carve is
//! blast-radius control: editing watcher code used to force a recheck
//! of ~18 crates because the watchers lived inside the corpus-engine
//! god-crate; now it rebuilds this crate + its handful of consumers.
//!
//! ## What lives here
//! - [`LintResultStore`] / [`TestResultStore`] — SQLite-backed stores
//!   for the latest lint/test run (the `lint_status`/`test_status`
//!   tools read these).
//! - [`WatcherCoordinator`] + [`BackgroundWatcher`] — the supervisor
//!   and the trait every watcher implements. [`ActivityCallback`] is
//!   the hook the coordinator fires on activity; [`WatcherHeartbeat`]
//!   is the liveness sidecar.
//! - [`LintWatcher`] / [`TestWatcher`] / [`ProjectIndexWatcher`] — the
//!   concrete watchers. They poll a shared [`corpus_engine_yield::YieldHook`]
//!   so their subprocess runs back off while the node serves
//!   foreground inference.
//!
//! ## What does NOT live here
//! The SCIP `CodeWatcher` (`corpus_engine::update::watch`) stays in
//! corpus-engine — it is genuinely tree-sitter/SCIP-coupled, unlike
//! these watchers (whose old `treesitter` gate was vestigial: they
//! only spawn `cargo` subprocesses and touch SQLite). This crate
//! therefore compiles unconditionally, with no feature flags.

pub mod error;
pub mod lint_results;
pub mod lint_watcher;
pub mod project_index_watcher;
pub mod test_results;
pub mod test_watcher;
pub mod watcher_coordinator;

pub use error::{Error, Result};

pub use lint_results::{LintResult, LintResultKind, LintResultStore, LintRunSummary};
pub use test_results::{RunSummary, TestResult, TestResultKind, TestResultStore};
pub use watcher_coordinator::{
    ActivityCallback, BackgroundWatcher, CoordinatorHandle, WatcherCoordinator, WatcherHeartbeat,
    WatcherStatus,
};

pub use lint_watcher::LintWatcher;
pub use project_index_watcher::ProjectIndexWatcher;
pub use test_watcher::TestWatcher;
