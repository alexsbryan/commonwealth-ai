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
pub mod cognitive;
pub mod diff;
pub mod disposition_bench;
pub mod disposition_score;
pub mod disposition_taxonomy;
pub mod entity_resolution_bench;
pub mod entity_resolution_score;
pub mod finalize;
pub mod judge;
pub mod manifest;
pub mod mechanical;
pub mod mechanism_fidelity;
pub mod regression;
pub mod scope;
pub mod tool_grader;
pub mod workflow;
