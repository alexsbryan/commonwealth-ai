// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas enrichment build — the orchestrator and the phase drivers it runs.
//!
//! `svrn enrich build` is a sequence of phases over one corpus (seed, extract,
//! cluster, name, resolve, tensions, gaps, configure, report, backfill). This
//! crate is that sequence and the drivers underneath it. It is a CAPABILITY,
//! not a command: three hosts run the same build — the CLI, the daemon
//! in-process for an `[enrichment] type = "atlas"` recipe, and the desktop,
//! whose shipped bundle carries no CLI to shell out to — so it sits below all
//! three rather than inside one of them (see `Cargo.toml` for the full why).
//!
//! ## What is here and what is not
//!
//! Each module keeps three parts of the verb triple: a `Parsed*` input, a
//! `run` that does the work and returns a report, and a `render` that prints
//! it. The fourth part — the `cmd_*` entry point and its `HELP` const — stays
//! in `sovereign-cli-llm`, because that is the `svrn enrich …` user interface:
//! usage strings, flag parsing, `--help`. A capability crate holding a host's
//! help text would be the layering mistake this split exists to correct.
//!
//! ## The embedder seam
//!
//! The Backfill step needs an embed provider. The caller supplies it —
//! the daemon passes its own `InferenceProvider` (so an in-process build never
//! opens an HTTP session back to itself), and the CLI resolves a daemon
//! session and passes that. This crate never builds a session of its own;
//! doing so is what used to tie the orchestrator to `sovereign-mesh` and three
//! other crates it has no business knowing about.

/// The enrichment store's layout and its `config.json` schema, re-exported so
/// this crate's modules reach them as `config` / `paths` — the same names
/// `enrich_cmd/mod.rs` re-exported before the move, which is what lets the
/// moved modules keep their `super::config` and `super::paths` paths verbatim.
pub use sovereign_enrichment_catalog::{config, paths};

/// The orchestrator: the plan, the steps, the cache gates.
pub mod build;

/// The two names a HOST needs to run an atlas build, at the crate root
/// because they are the interface this crate exists to offer: hand it a
/// `ParsedBuild` and an embed provider, get an exit code and a stream of
/// typed progress events. The daemon uses exactly these; `svrn enrich build`
/// wraps them with argv parsing and help text.
pub use build::{build_with_progress_with_embedder, ParsedBuild};

// The phase drivers the plan runs, each keeping the verb triple's `Parsed*`,
// `run` and `render`. Their `cmd_*` entry points and `HELP` consts stayed in
// `sovereign-cli-llm` — see this module's header.
pub mod atlas_configuration;
pub mod atlas_gaps;
pub mod atlas_phase_cmd;
pub mod atlas_resolve;
pub mod atlas_tensions;
pub mod atlas_tensions_classify;
pub mod extract;
pub mod schema_review;
pub mod seed_cmd;

// Shared plumbing. These carried no CLI half at all, so they moved whole and
// `enrich_cmd/mod.rs` re-exports them for the ~15 siblings that reach them.
pub mod corpus_io;
pub mod inference_client;
pub mod pipeline_resolve;
pub mod providers;
pub mod source_loader;

/// End-to-end tests for the build: a scaffolded corpus, deterministic embed
/// and canned chat closures, run through the real extract path. They came down
/// with the code they exercise (ontology-v1 P0.5) — they drive
/// `extract::run_with_closures_for_test`, which is `#[cfg(test)]`, and a
/// `_for_test` helper reaching across a crate boundary is a design smell, not
/// a visibility bug.
#[cfg(test)]
mod integration_tests;

/// The process-wide `HOME` lock shared with `sovereign-cli-llm`'s test
/// modules. Behind a feature so it costs a normal build nothing.
#[cfg(any(test, feature = "test-support"))]
pub mod test_env;
