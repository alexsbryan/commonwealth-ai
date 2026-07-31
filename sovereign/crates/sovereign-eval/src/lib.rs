// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tool-efficacy self-host harness.
//!
//! Reads what the daemon already records (`~/.sovereign/notes.db` and
//! `~/.sovereign/features.db`), assembles a per-overnight manifest,
//! grades the run mechanically (golden tests) + qualitatively
//! (LLM-as-judge), checks scope compliance + test regressions, replays
//! tool calls against a frozen oracle, and analyzes workflow +
//! audit-trail dimensions across run pairs.
//!
//! Operator-driven — no daemon source changes.

pub mod audit_trail;
// The authoring-harness verdict policy was extracted into its own light crate
// (`sovereign-authoring-harness`, deps: corpus-engine + serde only) so the
// desktop can consume it without dragging sovereign-eval's heavy deps
// (rusqlite-bundled, reqwest, clap) into the Tauri build. Re-exported here so
// `sovereign_eval::authoring_harness::*` keeps resolving for existing callers.
pub use sovereign_authoring_harness as authoring_harness;
pub mod chaos_monkey;
pub mod cognitive;
pub mod diff;
pub mod disposition_bench;
pub mod disposition_score;
pub mod disposition_taxonomy;
pub mod entity_resolution_bench;
pub mod entity_resolution_score;
pub mod faithfulness;
pub mod finalize;
pub mod flywheel;
pub mod governance_bench;
pub mod judge;
pub mod manifest;
pub mod mechanical;
pub mod mechanism_fidelity;
pub mod regression;
pub mod scope;
pub mod tool_grader;
pub mod workflow;
