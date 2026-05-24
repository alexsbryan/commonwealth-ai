//! Shared building blocks for every phase of the solver loop.
//!
//! Lifted from `sovereign-agent-bench/src/runners/shared.rs` +
//! `src/witness/test_result_parser.rs` so the TDD machine owns the
//! load-bearing primitives (EditAction, apply, snapshot, test
//! discovery, output parsing). Bench keeps working by depending on
//! `commonwealth-tdd` and re-exporting from here.
//!
//! Design notes (preserved from the 2026-05-24 search-not-agent
//! session — these are invariants, not preferences):
//!
//! 1. **No defensive parsing.** The model's emitted code lands
//!    verbatim. Pre-write syntax check rejects malformed.
//! 2. **Full directory snapshots.** Per-candidate workdir copies
//!    (not just source files) so tests directories + scaffolding
//!    survive the snapshot round-trip.
//! 3. **Tests as the only judge.** Test-pass count is the canonical
//!    fitness function. No LLM-eval, no rubric scoring.

pub mod apply;
pub mod edit;
pub mod lang;
pub mod parser;
pub mod snapshot;
pub mod source;
pub mod test_runner;

pub use apply::apply_edit;
pub use edit::{parse_response, EditAction, ParsedResponse};
pub use lang::Language;
pub use parser::{parse_test_output, TestParseResult};
pub use snapshot::snapshot_dir;
pub use source::{discover_source_file, render_with_line_numbers};
pub use test_runner::{run_tests, TestRunResult};
