//! Agent-coding battery — graded eight-problem benchmark.
//!
//! See `~/.claude/plans/i-want-to-pickup-sorted-eagle.md` for the
//! design rationale and ARCH_PRINCIPLES anchors. Quick orientation:
//!
//! - `runner` — `AgentRunner` trait + run-context + artifact (the seam).
//! - `runners` — concrete implementations + registry (`pi`, `mock`).
//! - `problem` — TOML schema, closed enums, loader.
//! - `witness` — auto-witness pipeline (fixture mount + verify_cmd
//!   + per-language test-result parser).
//! - `judge` / `judge_multi` — LLM-judge with N-trial majority vote.
//! - `scoring` — per-dim → per-problem → grand-total aggregation +
//!   regression delta.
//! - `report` — `BenchReport` JSON + text rollup.
//! - `baseline` — dated snapshot + `latest.json` symlink helpers.
//! - `sandbox` — TempDir + env-scrub guards.
//! - `cli` — `sovereign agent-bench <subcommand>` surface.

pub mod artifacts;
pub mod baseline;
pub mod cli;
pub mod failure_class;
pub mod judge;
pub mod judge_multi;
pub mod problem;
pub mod report;
pub mod runner;
pub mod runners;
pub mod sandbox;
pub mod scoring;
pub mod witness;

pub use cli::run_agent_bench;
