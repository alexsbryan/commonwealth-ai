//! v2 enrichment pipeline.
//!
//! Sibling to the v1 `Domain` + `FieldModelEngine` stack living in
//! `corpus-engine/src/enrichment/`. The v2 pipeline is the
//! permanent successor: it splits the monolithic 5-phase flow into
//! 7 per-phase LLM + clustering steps an admin CLI can iterate on
//! one at a time, with prompts shaped by a per-phase exemplar bank
//! instead of Rust string literals on each `Domain` impl.
//!
//! # Current status (Landing 1)
//!
//! - `SectionedChunker` + `ChapterManifest` — section-aware chunking
//!   with paragraph-level chunks, plus a stable per-corpus manifest.
//! - `Pipeline` trait + `PipelineRegistry` — string-id dispatch.
//! - `ExemplarBank` — JSON-backed exemplar storage with top-k
//!   cosine selection.
//! - `PhaseCache` + `RunOutputWriter` — atomic per-phase caches
//!   with mtime-based staleness, plus monotonic run output files.
//! - `LiteraryPipeline` — first concrete `Pipeline`, phase 1 fully
//!   implemented; phases 3/5/6/7 scaffolded with stub compose/parse
//!   methods landing in Landing 3.
//!
//! Landings 2+ add: per-phase runners (`runner.rs`), validation
//! battery (`validation.rs`), and the CLI admin harness under
//! `sovereign-cli/src/enrich_cmd/`.

pub mod chapter_manifest;
pub mod exemplar_bank;
pub mod phase_cache;
pub mod pipelines;
pub mod registry;
pub mod run_output;
pub mod runner;
pub mod trait_def;
pub mod types;
pub mod validation;
pub mod vector_clustering;

pub use chapter_manifest::{ChapterEntry, ChapterManifest};
pub use exemplar_bank::{Exemplar, ExemplarBank, ExemplarKind, ExemplarLint};
pub use phase_cache::{PhaseCache, PhaseCacheMeta, PhaseCacheStatus};
pub use registry::PipelineRegistry;
pub use run_output::RunOutputWriter;
pub use runner::{
    CascadeResult, CascadeStep, ChapterSelection, Phase1Progress,
    Phase1RunResult, Phase2RunResult, Phase3RunResult, Phase4RunResult, Phase5RunResult,
    Phase6RunResult, Phase7RunResult, PhaseFailure, PhaseRunResult, PhaseRunner,
};
pub use trait_def::Pipeline;
pub use validation::{
    run_battery, traverse_atlas, BatteryResult, BatteryRow, ConcernMatch, PositionRef,
    QueryBattery, QueryTraversal, TensionRef, ValidationQuestion,
};
pub use vector_clustering::{cluster_vectors, VectorClusterResult};
pub use types::{
    Atlas, CanonicalConcern, ChapterInput, ChatCompletionFn, ChatPrompt, ChunkCluster,
    ChunkRecord, CorpusContext, ExtractedQuestion, Gap, Grounding, Phase1ChapterResult,
    Phase1Failure, Phase1Output, Phase2Output, Phase3Output, Phase3ParseResult, Phase4Output,
    Phase5Output, Phase5ParseResult, Phase6Output, Phase6ParseResult, Phase7Output,
    Phase7ParseItem, PipelinePhase, Position, QuestionCluster, QuestionRef, Tension,
    Vocabulary, extract_json_block, is_placeholder_literal,
    is_truncated_thinking_response, strip_reasoning_tags,
};
