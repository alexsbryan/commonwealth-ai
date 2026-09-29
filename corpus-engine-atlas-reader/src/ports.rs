// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas family's port: what svrn's atlas tools ask ingest to write or
//! derive (pb-ingest-dial-tools-atlas, phase-b-43 family (4)).
//!
//! The leaf still writes nothing: this module holds the trait and the plain
//! values its methods name, and ingest's crate implements it. Each method is
//! addressed by path, the way the free functions it stands for were. svrn
//! holds an `Arc<dyn AtlasPort>` handed to it by whoever composed ingest,
//! beside the ports in `corpus_index::ingest_port` (FIVE_PROGRAMS §2c).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use corpus_index::types::EmbedFn;
use understanding_vocab::atoms::{AtomEnvelope, AtomsFile, Entity};
use understanding_vocab::edges::{Edge, EdgesFile};
use understanding_vocab::ontology::NavigationPolicy;

use crate::citation::SourceCitation;
use crate::raptor_read::RaptorSummaryRow;
use crate::summary::AtlasSummary;
use sovereign_contracts::daemon_wire::enrich::StarterQuestion;

/// A detected atom mention in a chunk's text: what
/// [`AtlasPort::detect_atom_spans`] answers with.
pub use understanding_atlas::atlas_traversal::spans::AtomSpan;

#[cfg(any(test, feature = "test-doubles"))]
pub mod double;

/// The atlas family's port. Ingest implements it once.
#[async_trait]
pub trait AtlasPort: Send + Sync {
    /// The atlas's `_summary.json`, computed and persisted when stale.
    /// `Ok(None)` when the atlas has no `atoms.json`.
    fn atlas_summary(&self, atlas_dir: &Path) -> io::Result<Option<AtlasSummary>>;

    /// Is the ANN seed table current for this atlas's atoms and population?
    fn ann_table_is_fresh(&self, atlas_dir: &Path) -> bool;

    /// Phase 7: detect the deterministic gaps over `atoms` and `edges` and
    /// write `gaps.json`. Returns the gap count and the written path.
    fn write_deterministic_gaps(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
        edges: &[Edge],
    ) -> io::Result<(usize, PathBuf)>;

    /// Phase 6, deterministic half: select the tension candidates over
    /// `atoms` and write `tension_candidates.json`. Returns the candidate
    /// count and the written path.
    fn write_tension_candidates(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
    ) -> io::Result<(usize, PathBuf)>;

    /// (Re)build `<corpus_dir>/raptor_summaries.lance` and its freshness
    /// sidecar from `rows`, stamped `source_version`. Returns rows written.
    async fn build_raptor_index(
        &self,
        corpus_dir: &Path,
        rows: &[RaptorSummaryRow],
        source_version: i64,
    ) -> corpus_index::Result<usize>;

    /// Every row of `<corpus_dir>/raptor_summaries.lance`, embeddings
    /// included. Empty when the table is absent; a table that is there and
    /// fails to read is an `Err`.
    async fn scan_raptor_summaries(
        &self,
        corpus_dir: &Path,
    ) -> corpus_index::Result<Vec<RaptorSummaryRow>>;

    /// The article title a RAPTOR `conv_uuid` names.
    fn raptor_article_title(&self, conv_uuid: &str) -> String;

    /// Write `edges.json`. Returns the written path.
    fn write_atlas_edges(&self, atlas_dir: &Path, edges: &EdgesFile) -> io::Result<PathBuf>;

    /// Write `atoms.json` and rebuild the atom store from it and the edges
    /// on disk. Returns the written path.
    fn write_atlas_atoms(&self, atlas_dir: &Path, atoms: &AtomsFile) -> io::Result<PathBuf>;

    /// Stamp the seed-population marker with the population this atlas
    /// derives now, so the next backfill keeps the seed table.
    fn write_population_marker(&self, atlas_dir: &Path) -> io::Result<()>;

    /// Run the `structure_first` atlas strategy over `corpus_id` (chunk
    /// metadata only, never embeds) and return the `(atoms, edges)` JSON it
    /// produced; the caller writes them. `Err` is the human-readable reason.
    async fn structural_atlas(
        &self,
        corpus_id: &str,
        indexes_dir: &Path,
        recipes_dir: &Path,
    ) -> Result<(serde_json::Value, serde_json::Value), String>;

    /// The Wikipedia vital-articles level (1-5) of a canonical name, if listed.
    fn vital_tier(&self, canonical_name: &str) -> Option<u8>;

    /// The title key ingest normalizes article titles to.
    fn normalize_title(&self, title: &str) -> String;

    /// The id and declared navigation map of a built-in pipeline, or `None`
    /// when no built-in pipeline has that id.
    fn pipeline_navigation(&self, pipeline_id: &str) -> Option<(String, NavigationPolicy)>;

    /// The system prompt of the argumentative typed-extension call.
    fn argumentative_system(&self) -> &'static str;

    /// The JSON schema the argumentative call's structured output obeys.
    fn argumentative_schema(&self) -> serde_json::Value;

    /// The verbatim-source block a typed-extension prompt carries.
    fn render_source_recovery_block(&self, excerpts: &[&str]) -> String;

    /// Parse one argumentative response (Pass B keeps only oppositions and
    /// concessions: `cross_leaf_only`) and count its atoms. `Err` is the
    /// parse error, for the call's retry.
    fn argumentative_atom_count(
        &self,
        response_text: &str,
        cross_leaf_only: bool,
    ) -> Result<usize, String>;

    /// Resolve the responses against `person_seeds`, rewrite ids to content
    /// hashes, fill passage previews from `citations` (keyed by section id)
    /// and write the atlas with its seed table, embedding through
    /// `embed_query`. Returns the atom count per kind.
    fn write_typed_extension(
        &self,
        corpus_id: &str,
        atlas_dir: &Path,
        responses: &[ArgumentativeResponse],
        person_seeds: Vec<Entity>,
        citations: &HashMap<String, SourceCitation>,
        embed_query: EmbedFn,
    ) -> corpus_index::Result<HashMap<String, u32>>;

    /// The atlas's `Question` atoms ranked as starter questions, best first,
    /// at most `limit`.
    fn rank_starter_questions(&self, atoms: &[AtomEnvelope], limit: usize) -> Vec<StarterQuestion>;

    /// The atom mentions in a chunk's `text`, anchored at its `section_id`
    /// (none without one).
    fn detect_atom_spans(
        &self,
        text: &str,
        section_id: Option<&str>,
        atoms: &[AtomEnvelope],
    ) -> Vec<AtomSpan>;

    /// Rewrite `atlas_dir`'s sequential atom ids to content hashes (nothing
    /// is written when `dry_run`); the migration summary, rendered for the
    /// log.
    fn migrate_atlas_ids(
        &self,
        atlas_dir: &Path,
        corpus_id: &str,
        dry_run: bool,
    ) -> Result<String, String>;
}

/// One typed-extension LLM response, carried to ingest as the model wrote
/// it; ingest parses it into its own section type at the write.
#[derive(Debug, Clone)]
pub struct ArgumentativeResponse {
    /// The section id the citation lookup is keyed on.
    pub section_id: String,
    /// The response the call accepted (it parsed).
    pub response_text: String,
    /// Pass B: keep only oppositions and concessions.
    pub cross_leaf_only: bool,
}
