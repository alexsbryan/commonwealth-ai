//! `AtlasIngestion` trait and the `AtlasData` bundle it returns.
//!
//! Every ingestion strategy — extraction-first LLM pipelines today,
//! a future structure-first Wikipedia parser tomorrow — implements
//! this single trait. The closed atlas surface (traversal engine,
//! brief assembler, schema validation) consumes `AtlasData` without
//! knowing which strategy produced it.
//!
//! The trait is intentionally small. Strategy-internal plumbing
//! (phase runners for extraction-first, section-header parsers for
//! structure-first) stays inside the impl; it doesn't leak onto the
//! trait.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::engine::CorpusEngine;
use crate::enrichment::pipeline::atlas::EnrichmentDepth;
use crate::error::Result;
use crate::progress::ProgressCallback;
use crate::types::{EmbedFn, InferenceFn};

/// Configuration passed to every ingestion strategy at invocation
/// time. Strategies reading this contract should treat unknown fields
/// as defaults — the config grows as new strategies surface their
/// knobs.
///
/// This intentionally starts minimal. Per-pipeline options (chapter
/// regex, exemplar directory, max_output_tokens) are plumbed through
/// `EnrichmentConfig` in `sovereign-cli::enrich_cmd::config.rs`;
/// strategies that need them pick them up via their own
/// configuration loader, not this struct.
#[derive(Debug, Clone, Default)]
pub struct AtlasIngestionConfig {
    /// Ingestion strategy id (`"extraction_first"`,
    /// `"structure_first"`, …). The registry matches on this.
    pub strategy_id: String,
    /// Per-strategy opaque config blob. Strategies deserialise this
    /// as their own typed config; the trait stays generic.
    pub strategy_config: serde_json::Value,
}

/// The output of a single ingestion run. The closed atlas surface
/// consumes this and writes it to disk (`atlas/*.json`); nothing in
/// the consumer path depends on *how* the bundle was assembled.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AtlasData {
    /// All atoms produced by this run. Schema version pinned by the
    /// atoms module when it lands in Phase A Step 3a.
    pub atoms: serde_json::Value,
    /// All intra-corpus edges. Cross-corpus edges live in their own
    /// file (`atlas/cross_corpus_edges.json`), not here.
    pub edges: serde_json::Value,
    /// Pre-computed per-entity and per-relation state sequences.
    /// Empty until Phase A Step 3b fills it.
    pub trajectories: serde_json::Value,
    /// Topic manifest (§4). Empty until Phase A Step 6.
    pub manifest: serde_json::Value,
    /// Schema-validation diagnostics (§12). Incrementally written
    /// across phases.
    pub schema_validation: serde_json::Value,
    /// Summary tag describing the overall enrichment depth of this
    /// atlas — the brief assembler reads this for per-corpus
    /// language calibration alongside per-atom depth tags.
    pub dominant_depth: EnrichmentDepth,
}

/// Produce atlas atoms + edges from a corpus.
///
/// Implementations may use structural parsing, LLM extraction, or
/// both. The output schema (`AtlasData`) is identical regardless of
/// strategy; the difference shows up in the `enrichment_depth` tags
/// carried by individual atoms inside the JSON payload.
///
/// The trait is object-safe (`&self`, heap-returned future). Held
/// as `Arc<dyn AtlasIngestion>` in the registry.
pub trait AtlasIngestion: Send + Sync {
    /// Stable short id used in config + CLI selection.
    fn id(&self) -> &'static str;

    /// Human-readable name (shown in status output).
    fn name(&self) -> &'static str;

    /// Run the full ingestion. Takes `&self` so a registered
    /// strategy can be invoked concurrently across corpora. The
    /// returned future is `Send + 'static` so callers can spawn it.
    fn ingest<'a>(
        &'a self,
        corpus: Arc<CorpusEngine>,
        embed_fn: EmbedFn,
        inference_fn: Option<InferenceFn>,
        config: AtlasIngestionConfig,
        progress: Arc<ProgressCallback>,
    ) -> Pin<Box<dyn Future<Output = Result<AtlasData>> + Send + 'a>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal smoke test — the trait is object-safe and
    /// `AtlasData` roundtrips through serde.
    #[test]
    fn atlas_data_roundtrips_through_serde() {
        let data = AtlasData {
            atoms: serde_json::json!([]),
            edges: serde_json::json!([]),
            trajectories: serde_json::json!({}),
            manifest: serde_json::json!({}),
            schema_validation: serde_json::json!({}),
            dominant_depth: EnrichmentDepth::Extracted,
        };
        let json = serde_json::to_string(&data).unwrap();
        let parsed: AtlasData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.dominant_depth, EnrichmentDepth::Extracted);
    }

    #[test]
    fn atlas_ingestion_trait_is_object_safe() {
        // Compile-time check: if this function type-checks, the
        // trait is object-safe and can be held behind `dyn`.
        fn _accepts_dyn(_t: &dyn AtlasIngestion) {}
    }
}
