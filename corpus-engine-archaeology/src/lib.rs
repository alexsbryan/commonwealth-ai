// SPDX-License-Identifier: AGPL-3.0-or-later
//! # corpus-engine-archaeology
//!
//! Git-history mining + rough-edge surfacing + inquiry tracking.
//!
//! Carved out of `corpus-engine` (2026-05-23, step 4 of the
//! decomposition plan). The three modules are independent of the
//! corpus pipeline — they read git, parse repository state, and
//! surface findings for the agent to act on. Three consumers
//! (`sovereign-cli` for the CLI commands, `sovereign-tools` for the
//! agent brief, `sovereign-cli-llm` for one drift-report renderer)
//! depend here directly.
//!
//! Modules:
//! - [`git_archaeology`] — Commit-history harvesting + atom-provenance
//!   (which commit touched which atom). Defines `GitArchaeologyError`
//!   locally — the three modules share it rather than reaching into
//!   any shared error enum.
//! - [`archaeology_eval`] — Maps recorded inquiries (e.g.
//!   `.sovereign/inquiries/*.toml`) onto commits and files for the
//!   archaeology CLI evaluator.
//! - [`rough_edges`] — Finds TODOs / FIXMEs / red-team flags that
//!   accumulate without resolution.

pub mod archaeology_eval;
pub mod git_archaeology;
pub mod rough_edges;
