// SPDX-License-Identifier: AGPL-3.0-or-later
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

pub mod assemble;
pub mod atlas;
pub mod atlas_clustering;
pub mod atom_normalizer;
pub mod chapter_manifest;
pub mod exemplar_bank;
pub mod phase_cache;
pub mod pipelines;
pub mod progress;
pub mod prompts;
pub mod registry;
pub mod run_output;
pub mod runner;
pub mod section_classifier;
pub mod section_join;
pub mod text_helpers;
pub mod trait_def;
pub mod typed_schemas;
pub mod types;
pub mod validation;
pub mod vector_clustering;

pub use assemble::assemble_phase_output;
pub use atlas::{
    ClaimScope, ClaimSketch, DiscourseAct, EnrichmentDepth, EntitySketch, EntityStateSketch,
    EntityType, EpistemicStatus, EventSketch, EventType, QuestionSketch, QuestionType,
    RelationSketch, RelationStateSketch, RelationType, SectionExtraction, SeedEntities, SeedEntity,
    SeedOrigin, SeedStrategy, StateType,
};
pub use atlas_clustering::{cluster_all_facets, cluster_facet, FacetClusterResult};
pub use chapter_manifest::{ChapterEntry, ChapterManifest};
pub use exemplar_bank::{Exemplar, ExemplarBank, ExemplarKind, ExemplarLint};
pub use phase_cache::{CacheModelIdentity, PhaseCache, PhaseCacheMeta, PhaseCacheStatus};
pub use pipelines::configurable_atlas::{CustomAtlasSpec, CustomVocabulary};
pub use progress::{BuildStep, EnrichProgress, EnrichProgressFn};
pub use registry::PipelineRegistry;
pub use run_output::RunOutputWriter;
pub use runner::{
    checkpoint_processed_ids, collapse_phase1_checkpoint, read_phase1_checkpoint, CascadeResult,
    CascadeStep, ChapterSelection, Phase1CheckpointEntry, Phase1Progress, Phase1RunResult,
    Phase2AtlasRunResult, Phase2RunResult, Phase3RunResult, Phase4RunResult, Phase5RunResult,
    Phase6RunResult, Phase7RunResult, PhaseFailure, PhaseRunResult, PhaseRunner,
};
pub use section_join::{assign_chunks_to_sections, SectionJoin};
pub use trait_def::Pipeline;
pub use types::{
    extract_json_block, is_placeholder_literal, is_truncated_thinking_response,
    strip_reasoning_tags, Atlas, AtlasCluster, CanonicalConcern, ChapterInput, ChatCompletionFn,
    ChatCompletionWithTokensFn, ChatPrompt, ChunkCluster, ChunkRecord, CorpusContext, ExtractedGap,
    ExtractedQuestion, Facet, Grounding, NamedCluster, Phase1ChapterResult, Phase1Failure,
    Phase1Output, Phase2AtlasOutput, Phase2Output, Phase3AtlasOutput, Phase3FacetParseResult,
    Phase3Output, Phase3ParseResult, Phase4Output, Phase5Output, Phase5ParseResult, Phase6Output,
    Phase6ParseResult, Phase7Output, Phase7ParseItem, PhaseFailureKind, PipelinePhase, Position,
    QuestionCluster, QuestionRef, RetryMode, SketchExcerpt, SketchRef, Tension, Vocabulary,
};
pub use validation::{
    run_battery, traverse_atlas, BatteryResult, BatteryRow, ConcernMatch, PositionRef,
    QueryBattery, QueryTraversal, TensionRef, ValidationQuestion,
};
pub use vector_clustering::{cluster_vectors, VectorClusterResult};
