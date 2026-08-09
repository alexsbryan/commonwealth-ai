// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native grounding — H4 mechanical attribution (`NATIVE_GROUNDING.md` §5 H4).
//!
//! **This module has zero callers in the runtime, by construction.** It is the
//! offline measurement surface for the H4 gate (§7.3): the span resolver and the
//! sentence-margin sweep, built and pinned before anything is wired. The
//! incumbent grounding stack (`gate_longform`, the citation path,
//! `classify_caveat`) is untouched — H4's replay *reads* that stack's frozen
//! outputs and never edits its code. Cutover, if the gate holds, is a later
//! order.
//!
//! **Why it lives inside `runtime::grounding` and not in `sovereign-eval`.**
//! Principle 11 (the inventory outranks the plan) says reuse what exists; §5 H4
//! names four existing surfaces this composes, and every one of them is
//! module-private to `grounding`:
//!
//! | Reused surface | Where | Visibility |
//! |---|---|---|
//! | presence kernel `value_present_in_chunks` | `value_presence.rs:152` | `pub` inside a private `mod` |
//! | lossless sentence splitter | `surgical.rs:42` | `pub(super)` |
//! | deterministic name veto | `judge.rs:890` | `pub(super)` |
//! | deterministic identifier veto | `judge.rs:974` | `pub(super)` |
//!
//! A descendant module of `grounding` sees all four with **zero edits to any
//! existing file**. `sovereign-eval` — the other placement the order offered —
//! does not depend on `sovereign-core` at all (`sovereign-eval/Cargo.toml` has
//! no such line), so siting the code there would have meant either a new crate
//! dependency across the layer map or re-deriving four shipped deciders. The
//! second is principle 8's smell verbatim ("two implementations of one
//! threshold, formula, or key"). The registration cost of this placement is two
//! additive lines: `mod native_grounding;` here and one `pub use` in
//! `runtime.rs`.
//!
//! **The reranker is injected, not imported.** `sovereign-core` does not depend
//! on `sovereign-inference`, and it should not start to for a measurement. The
//! sweep takes a [`sentence_sweep::SentenceScorer`] trait object; the CLI
//! harness (`sovereign-cli-llm/src/bench_cmd/h4/`) supplies the real
//! `StandaloneReranker`, and the tests supply a deterministic fake. That keeps
//! the determinism pin runnable with no model on disk.
//!
//! **No thresholds live here.** The sweep reports margins; the floor is
//! calibrated in the H4 gate and committed beside the code that reads it
//! (principle 2, §7.1's "a threshold with no committed curve fails review").

pub mod meaning_cluster;
pub mod sentence_sweep;
pub mod span_resolver;
