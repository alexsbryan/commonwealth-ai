// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod atlas_context_manager;
pub mod atlas_peer_advice;
pub mod atlas_phase;
pub mod atlas_postinstall;
pub mod atlas_status;
pub mod atlas_view;
pub mod attached_document_search;
pub mod bundles;
pub mod calendar;
pub mod catalog;
pub mod catalog_ingest;
pub mod compute;
pub mod conv_tiered_provider;
pub mod corpus;
pub mod corpus_search;
pub mod corpus_store;
pub mod document;
pub mod document_asset;
pub mod document_operation;
pub mod email;
pub mod enrich;
pub mod enrichment_bootstrap;
pub mod enrichment_checker;
pub mod entity_graph;
pub mod epistemic;
pub mod extract;
pub mod file;
pub mod knowledge;
pub mod knowledge_lookup;
pub mod knowledge_view;
pub mod local_corpus;
pub mod mem_atlas;
pub mod mem_tree;
pub mod raptor_atlas;
pub mod raptor_checkpoint;
pub mod raptor_index;
pub mod summary_atoms;
pub mod summary_verify;
pub use sovereign_tools_base::read_csv;
pub use sovereign_tools_base::vector_mean;
pub mod wikipedia_fetch;
// `manifest` module retired 2026-05-22 — commonwealth-api now
// injects tool descriptors at construction time rather than pulling
// from a global static. See `commonwealth-api::middleware::tool_injector`
// and `context_injector` for the new shape.
pub use sovereign_tools_base::mcp;
pub mod mcp_surface;
pub mod parcel_analytics;
pub mod rag;
pub mod sec_edgar;
pub mod sec_facts;
/// THE one decider for SEC XBRL companyfacts: turns a raw companyfacts
/// document + the concept-normalization registry into the ingested
/// `facts/*.txt` lines and the typed `sec_facts.json` sidecar that
/// `sec_facts` (above) answers from. Ported from `scripts/sec_facts.py`,
/// which was deleted in the same commit — one decider, one name
/// (ARCH §10.6).
pub mod sec_facts_render;
pub use sovereign_tools_base::read_file;
pub use sovereign_tools_base::read_json;
pub use sovereign_tools_base::search;
pub use sovereign_tools_base::shell;
pub mod typed_call;
pub mod typed_extension;
pub use sovereign_tools_base::web;
pub use sovereign_tools_base::write_file;
pub use sovereign_tools_base::write_json;
pub use sovereign_tools_base::zip;

pub use attached_document_search::AttachedDocumentSearchTool;
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
#[cfg(feature = "treesitter")]
pub use document_asset::DocumentAssetManager;
pub use document_operation::DocumentOperationTool;
pub use epistemic::{ClaimSearchTool, EpistemicLandscapeTool};
pub use knowledge_lookup::{
    Evidence, EvidenceId, EvidenceKind, KindCounts, KnowledgeLookupResponse, KnowledgeLookupTool,
    TOOL_DESCRIPTION as KNOWLEDGE_LOOKUP_TOOL_DESCRIPTION,
};
pub use sovereign_core;
pub use wikipedia_fetch::WikipediaFetchTool;

/// The workflow tools that intentionally do NOT live in
/// `sovereign-tools-base`. `sovereign-workflow-host`'s `standard_registry`
/// therefore registers only the ones that do; every call site that runs
/// workflows and wants the *full* surface (the CLI `workflow run` /
/// `corpus ingest`, the desktop run/ingest commands, the daemon
/// living-trigger) injects these through the runner's `extra_tools` slot.
///
/// Five of the six are out for the original reason: each drags
/// corpus-engine/LanceDB, which is exactly what the base bundle exists to
/// avoid. `section` is out for a DIFFERENT reason and joined this list at
/// sv-surface svt-7 (2026-09-12): its `corpus-engine-sections` dependency is a
/// `regex` leaf and weighs nothing, but tools-base is the one runtime crate a
/// THIN SURFACE may link, so what tools-base links a thin client links. That
/// one edge was the last `from = "sovereign-desktop"` row in
/// quality/ARCH_LAYERS.toml. See `rag/mod.rs`.
///
/// Kept here — beside the tools themselves — so "which six" is stated once
/// rather than re-listed (and drifting) at each injection site. Registration
/// order is irrelevant: the registry keys on tool id and these ids are distinct
/// from the base set, so injecting them via `extra_tools` reproduces exactly the
/// pre-extraction registry.
pub fn workflow_corpus_tools(
    atlas: std::sync::Arc<dyn corpus_engine_atlas_reader::ports::AtlasPort>,
) -> Vec<Box<dyn sovereign_core::traits::Tool>> {
    use std::sync::Arc;
    vec![
        Box::new(extract::ExtractTool.declared()),
        Box::new(corpus_store::CorpusStoreTool.declared()),
        Box::new(corpus_search::CorpusSearchTool.declared()),
        Box::new(atlas_phase::gaps::AtlasGapsTool::new(Arc::clone(&atlas)).declared()),
        Box::new(atlas_phase::tensions::AtlasTensionsTool::new(atlas).declared()),
        Box::new(rag::section::SectionTool),
    ]
}
