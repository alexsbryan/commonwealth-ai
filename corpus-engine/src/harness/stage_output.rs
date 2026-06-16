// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed, judgment-free observations the runner emits per stage. The policy
//! layer (`sovereign-eval::authoring_harness`) turns these into Pass/Fail
//! verdicts — nothing here decides anything.

use crate::extractors::ExtractedDoc;

use super::miss::FieldMiss;

/// Extract stage — the docs the extractor produced over the frozen sample.
pub struct ExtractOutput {
    pub docs: Vec<ExtractedDoc>,
    /// How many docs the extractor yielded before the sample bound (counts
    /// those that errored too).
    pub attempted: usize,
    /// First few per-doc extraction errors, for evidence.
    pub errors: Vec<String>,
    /// Distinct source files in the frozen sample — the denominator for
    /// section-extractor per-file coverage.
    pub source_files: usize,
    /// Section misses slurped from the extractor's `_section_misses.json`
    /// sidecar (html_sections only; empty otherwise).
    pub section_misses: Vec<FieldMiss>,
}

/// Filter stage — which sample docs the document-level filters kept vs dropped.
pub struct FilterOutput {
    pub active: bool,
    pub kept: Vec<ExtractedDoc>,
    pub dropped: Vec<ExtractedDoc>,
    /// Human descriptions of each active filter (the firing predicates).
    pub descriptions: Vec<String>,
}

/// Chunk stage — the chunk texts produced from the kept docs.
pub struct ChunkOutput {
    /// Every chunk's title-prepended text, in document order.
    pub chunks: Vec<String>,
    /// Chunks produced per kept doc — for the "collapsed to a single chunk"
    /// degeneracy check.
    pub per_doc_counts: Vec<usize>,
    /// The chunker's declared upper bound (`max_chars`); `usize::MAX` for
    /// unbounded chunkers (passthrough / threaded_turns).
    pub declared_max_chars: usize,
}

/// Index stage — a tiny FTS index built from the chunks, with a rare-token
/// round-trip. Model-free: zero-vectors at a fixed dim, FTS-only build.
pub struct IndexOutput {
    /// The index built and opened cleanly.
    pub built: bool,
    /// Embed model the recipe declares (`[index].embedding_model`).
    pub model_declared: String,
    /// Embed model the built index recorded.
    pub model_recorded: String,
    /// The deterministically-chosen rare token used for the round-trip.
    pub token: Option<String>,
    /// Preview of the chunk the token was drawn from.
    pub source_preview: Option<String>,
    /// The token, FTS-queried, returned its own source chunk.
    pub roundtrip_ok: bool,
    /// How many hits the FTS query returned.
    pub hit_count: usize,
    /// Set when any step (create / insert / build / search) errored.
    pub error: Option<String>,
}

/// All per-stage observations from one harness run over the frozen sample.
/// The Enrich output is added in a later increment.
pub struct StageOutputs {
    pub extract: ExtractOutput,
    pub filter: FilterOutput,
    pub chunk: ChunkOutput,
    pub index: IndexOutput,
}
