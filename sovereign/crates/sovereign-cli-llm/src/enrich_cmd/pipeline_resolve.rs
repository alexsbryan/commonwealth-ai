// SPDX-License-Identifier: AGPL-3.0-or-later
//! Single source of truth for turning an [`EnrichConfig`] into a live
//! `Arc<dyn Pipeline>`.
//!
//! Every enrich subcommand that drives a pipeline (build, extract, seed,
//! cluster, cascade, phase, atlas-*) MUST resolve through [`resolve_pipeline`]
//! rather than calling `PipelineRegistry::builtin().get(&cfg.pipeline_id)`
//! directly. A recipe-customized atlas (`cfg.ontology`, the `custom_atlas`
//! pipeline) is built from DATA and is deliberately NOT in the registry — so a
//! site that bypasses this helper would silently fail to find it. Keeping the
//! resolution in one place is what lets "author your domain ontology" work the
//! same across every step of the build.

use std::sync::Arc;

use corpus_engine::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
use corpus_engine::enrichment::pipeline::{Pipeline, PipelineRegistry};

use super::config::EnrichConfig;

/// Resolve the pipeline for an enrich config.
///
/// - `cfg.ontology` present → build a recipe-customized atlas pipeline from the
///   spec (domain guidance → neutral Phase-1 prompt; identical downstream).
/// - otherwise → look `cfg.pipeline_id` up in the builtin registry.
///
/// Returns `None` only when a non-custom `pipeline_id` is unknown — same
/// contract as `PipelineRegistry::get`, so call sites keep their existing
/// "unknown pipeline" error handling.
pub fn resolve_pipeline(cfg: &EnrichConfig) -> Option<Arc<dyn Pipeline>> {
    if let Some(spec) = &cfg.ontology {
        return Some(
            Arc::new(LiteraryAtlasPipeline::with_custom_ontology(spec)) as Arc<dyn Pipeline>
        );
    }
    PipelineRegistry::builtin().get(&cfg.pipeline_id)
}
