// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one test double of [`AtlasPort`] (FIVE_PROGRAMS "Where a cross-program
//! test lives", phase-b-47). A svrn test drives its own code against this
//! and asserts on what that code does with the port; the port's behaviour is
//! proven on `impl AtlasPort for IngestAtlas`, in corpus-engine's tests.
//!
//! Each method answers through the handler a test programmed with `on_*`. An
//! unprogrammed method never answers success-shaped (principle 6): a
//! `Result` method returns an `Err` naming itself, and any other method
//! panics naming itself. Every call is recorded by method name, in order.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use corpus_index::types::EmbedFn;
use understanding_vocab::atoms::{AtomEnvelope, AtomsFile, Entity};
use understanding_vocab::edges::{Edge, EdgesFile};
use understanding_vocab::ontology::NavigationPolicy;

use super::{ArgumentativeResponse, AtlasPort, AtomSpan};
use crate::citation::SourceCitation;
use crate::raptor_read::RaptorSummaryRow;
use crate::summary::AtlasSummary;
use sovereign_contracts::daemon_wire::enrich::StarterQuestion;

type H<F> = Option<Box<F>>;

/// The [`AtlasPort`] a svrn test programs, method by method.
#[derive(Default)]
pub struct AtlasPortDouble {
    calls: Mutex<Vec<&'static str>>,
    atlas_summary: H<dyn Fn(&Path) -> io::Result<Option<AtlasSummary>> + Send + Sync>,
    ann_table_is_fresh: H<dyn Fn(&Path) -> bool + Send + Sync>,
    write_deterministic_gaps:
        H<dyn Fn(&Path, &[AtomEnvelope], &[Edge]) -> io::Result<(usize, PathBuf)> + Send + Sync>,
    write_tension_candidates:
        H<dyn Fn(&Path, &[AtomEnvelope]) -> io::Result<(usize, PathBuf)> + Send + Sync>,
    build_raptor_index:
        H<dyn Fn(&Path, &[RaptorSummaryRow], i64) -> corpus_index::Result<usize> + Send + Sync>,
    scan_raptor_summaries:
        H<dyn Fn(&Path) -> corpus_index::Result<Vec<RaptorSummaryRow>> + Send + Sync>,
    raptor_article_title: H<dyn Fn(&str) -> String + Send + Sync>,
    write_atlas_edges: H<dyn Fn(&Path, &EdgesFile) -> io::Result<PathBuf> + Send + Sync>,
    write_atlas_atoms: H<dyn Fn(&Path, &AtomsFile) -> io::Result<PathBuf> + Send + Sync>,
    write_population_marker: H<dyn Fn(&Path) -> io::Result<()> + Send + Sync>,
    structural_atlas: H<
        dyn Fn(&str, &Path, &Path) -> Result<(serde_json::Value, serde_json::Value), String>
            + Send
            + Sync,
    >,
    vital_tier: H<dyn Fn(&str) -> Option<u8> + Send + Sync>,
    normalize_title: H<dyn Fn(&str) -> String + Send + Sync>,
    pipeline_navigation: H<dyn Fn(&str) -> Option<(String, NavigationPolicy)> + Send + Sync>,
    argumentative_system: Option<&'static str>,
    argumentative_schema: H<dyn Fn() -> serde_json::Value + Send + Sync>,
    render_source_recovery_block: H<dyn Fn(&[&str]) -> String + Send + Sync>,
    argumentative_atom_count: H<dyn Fn(&str, bool) -> Result<usize, String> + Send + Sync>,
    #[allow(clippy::type_complexity)]
    write_typed_extension: H<
        dyn Fn(
                &str,
                &Path,
                &[ArgumentativeResponse],
                Vec<Entity>,
                &HashMap<String, SourceCitation>,
            ) -> corpus_index::Result<HashMap<String, u32>>
            + Send
            + Sync,
    >,
    rank_starter_questions: H<dyn Fn(&[AtomEnvelope], usize) -> Vec<StarterQuestion> + Send + Sync>,
    detect_atom_spans:
        H<dyn Fn(&str, Option<&str>, &[AtomEnvelope]) -> Vec<AtomSpan> + Send + Sync>,
    migrate_atlas_ids: H<dyn Fn(&Path, &str, bool) -> Result<String, String> + Send + Sync>,
}

fn unprogrammed(method: &str) -> String {
    format!("AtlasPortDouble::{method}: not programmed by this test")
}

impl AtlasPortDouble {
    /// A double with no method programmed.
    pub fn new() -> Self {
        Self::default()
    }

    /// The methods called so far, by name, in order.
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn record(&self, method: &'static str) {
        self.calls.lock().expect("calls lock").push(method);
    }

    /// Program [`AtlasPort::atlas_summary`].
    pub fn on_atlas_summary(
        mut self,
        f: impl Fn(&Path) -> io::Result<Option<AtlasSummary>> + Send + Sync + 'static,
    ) -> Self {
        self.atlas_summary = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::atlas_summary`] with this leaf's read half:
    /// `Ok(None)` without an `atoms.json`, else [`crate::summary::compute_summary`].
    /// It never persists `_summary.json`; that cache is ingest's, proven on
    /// `IngestAtlas`.
    pub fn with_computed_summaries(self) -> Self {
        self.on_atlas_summary(|atlas_dir| {
            if !atlas_dir.join("atoms.json").exists() {
                return Ok(None);
            }
            crate::summary::compute_summary(atlas_dir).map(Some)
        })
    }

    /// Program [`AtlasPort::ann_table_is_fresh`].
    pub fn on_ann_table_is_fresh(
        mut self,
        f: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.ann_table_is_fresh = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::write_deterministic_gaps`].
    pub fn on_write_deterministic_gaps(
        mut self,
        f: impl Fn(&Path, &[AtomEnvelope], &[Edge]) -> io::Result<(usize, PathBuf)>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.write_deterministic_gaps = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::write_tension_candidates`].
    pub fn on_write_tension_candidates(
        mut self,
        f: impl Fn(&Path, &[AtomEnvelope]) -> io::Result<(usize, PathBuf)> + Send + Sync + 'static,
    ) -> Self {
        self.write_tension_candidates = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::build_raptor_index`].
    pub fn on_build_raptor_index(
        mut self,
        f: impl Fn(&Path, &[RaptorSummaryRow], i64) -> corpus_index::Result<usize>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.build_raptor_index = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::scan_raptor_summaries`].
    pub fn on_scan_raptor_summaries(
        mut self,
        f: impl Fn(&Path) -> corpus_index::Result<Vec<RaptorSummaryRow>> + Send + Sync + 'static,
    ) -> Self {
        self.scan_raptor_summaries = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::raptor_article_title`].
    pub fn on_raptor_article_title(
        mut self,
        f: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        self.raptor_article_title = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::write_atlas_edges`].
    pub fn on_write_atlas_edges(
        mut self,
        f: impl Fn(&Path, &EdgesFile) -> io::Result<PathBuf> + Send + Sync + 'static,
    ) -> Self {
        self.write_atlas_edges = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::write_atlas_atoms`].
    pub fn on_write_atlas_atoms(
        mut self,
        f: impl Fn(&Path, &AtomsFile) -> io::Result<PathBuf> + Send + Sync + 'static,
    ) -> Self {
        self.write_atlas_atoms = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::write_population_marker`].
    pub fn on_write_population_marker(
        mut self,
        f: impl Fn(&Path) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.write_population_marker = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::structural_atlas`].
    pub fn on_structural_atlas(
        mut self,
        f: impl Fn(&str, &Path, &Path) -> Result<(serde_json::Value, serde_json::Value), String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.structural_atlas = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::vital_tier`].
    pub fn on_vital_tier(mut self, f: impl Fn(&str) -> Option<u8> + Send + Sync + 'static) -> Self {
        self.vital_tier = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::normalize_title`].
    pub fn on_normalize_title(
        mut self,
        f: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        self.normalize_title = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::pipeline_navigation`].
    pub fn on_pipeline_navigation(
        mut self,
        f: impl Fn(&str) -> Option<(String, NavigationPolicy)> + Send + Sync + 'static,
    ) -> Self {
        self.pipeline_navigation = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::argumentative_system`].
    pub fn on_argumentative_system(mut self, system: &'static str) -> Self {
        self.argumentative_system = Some(system);
        self
    }

    /// Program [`AtlasPort::argumentative_schema`].
    pub fn on_argumentative_schema(
        mut self,
        f: impl Fn() -> serde_json::Value + Send + Sync + 'static,
    ) -> Self {
        self.argumentative_schema = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::render_source_recovery_block`].
    pub fn on_render_source_recovery_block(
        mut self,
        f: impl Fn(&[&str]) -> String + Send + Sync + 'static,
    ) -> Self {
        self.render_source_recovery_block = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::argumentative_atom_count`].
    pub fn on_argumentative_atom_count(
        mut self,
        f: impl Fn(&str, bool) -> Result<usize, String> + Send + Sync + 'static,
    ) -> Self {
        self.argumentative_atom_count = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::write_typed_extension`] (the embedder is not
    /// handed on: a double embeds nothing).
    pub fn on_write_typed_extension(
        mut self,
        f: impl Fn(
                &str,
                &Path,
                &[ArgumentativeResponse],
                Vec<Entity>,
                &HashMap<String, SourceCitation>,
            ) -> corpus_index::Result<HashMap<String, u32>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.write_typed_extension = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::rank_starter_questions`].
    pub fn on_rank_starter_questions(
        mut self,
        f: impl Fn(&[AtomEnvelope], usize) -> Vec<StarterQuestion> + Send + Sync + 'static,
    ) -> Self {
        self.rank_starter_questions = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::detect_atom_spans`].
    pub fn on_detect_atom_spans(
        mut self,
        f: impl Fn(&str, Option<&str>, &[AtomEnvelope]) -> Vec<AtomSpan> + Send + Sync + 'static,
    ) -> Self {
        self.detect_atom_spans = Some(Box::new(f));
        self
    }

    /// Program [`AtlasPort::migrate_atlas_ids`].
    pub fn on_migrate_atlas_ids(
        mut self,
        f: impl Fn(&Path, &str, bool) -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        self.migrate_atlas_ids = Some(Box::new(f));
        self
    }
}

#[async_trait]
impl AtlasPort for AtlasPortDouble {
    fn atlas_summary(&self, atlas_dir: &Path) -> io::Result<Option<AtlasSummary>> {
        self.record("atlas_summary");
        match &self.atlas_summary {
            Some(f) => f(atlas_dir),
            None => Err(io::Error::other(unprogrammed("atlas_summary"))),
        }
    }

    fn ann_table_is_fresh(&self, atlas_dir: &Path) -> bool {
        self.record("ann_table_is_fresh");
        match &self.ann_table_is_fresh {
            Some(f) => f(atlas_dir),
            None => panic!("{}", unprogrammed("ann_table_is_fresh")),
        }
    }

    fn write_deterministic_gaps(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
        edges: &[Edge],
    ) -> io::Result<(usize, PathBuf)> {
        self.record("write_deterministic_gaps");
        match &self.write_deterministic_gaps {
            Some(f) => f(atlas_dir, atoms, edges),
            None => Err(io::Error::other(unprogrammed("write_deterministic_gaps"))),
        }
    }

    fn write_tension_candidates(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
    ) -> io::Result<(usize, PathBuf)> {
        self.record("write_tension_candidates");
        match &self.write_tension_candidates {
            Some(f) => f(atlas_dir, atoms),
            None => Err(io::Error::other(unprogrammed("write_tension_candidates"))),
        }
    }

    async fn build_raptor_index(
        &self,
        corpus_dir: &Path,
        rows: &[RaptorSummaryRow],
        source_version: i64,
    ) -> corpus_index::Result<usize> {
        self.record("build_raptor_index");
        match &self.build_raptor_index {
            Some(f) => f(corpus_dir, rows, source_version),
            None => Err(corpus_index::Error::Io(io::Error::other(unprogrammed(
                "build_raptor_index",
            )))),
        }
    }

    async fn scan_raptor_summaries(
        &self,
        corpus_dir: &Path,
    ) -> corpus_index::Result<Vec<RaptorSummaryRow>> {
        self.record("scan_raptor_summaries");
        match &self.scan_raptor_summaries {
            Some(f) => f(corpus_dir),
            None => Err(corpus_index::Error::Io(io::Error::other(unprogrammed(
                "scan_raptor_summaries",
            )))),
        }
    }

    fn raptor_article_title(&self, conv_uuid: &str) -> String {
        self.record("raptor_article_title");
        match &self.raptor_article_title {
            Some(f) => f(conv_uuid),
            None => panic!("{}", unprogrammed("raptor_article_title")),
        }
    }

    fn write_atlas_edges(&self, atlas_dir: &Path, edges: &EdgesFile) -> io::Result<PathBuf> {
        self.record("write_atlas_edges");
        match &self.write_atlas_edges {
            Some(f) => f(atlas_dir, edges),
            None => Err(io::Error::other(unprogrammed("write_atlas_edges"))),
        }
    }

    fn write_atlas_atoms(&self, atlas_dir: &Path, atoms: &AtomsFile) -> io::Result<PathBuf> {
        self.record("write_atlas_atoms");
        match &self.write_atlas_atoms {
            Some(f) => f(atlas_dir, atoms),
            None => Err(io::Error::other(unprogrammed("write_atlas_atoms"))),
        }
    }

    fn write_population_marker(&self, atlas_dir: &Path) -> io::Result<()> {
        self.record("write_population_marker");
        match &self.write_population_marker {
            Some(f) => f(atlas_dir),
            None => Err(io::Error::other(unprogrammed("write_population_marker"))),
        }
    }

    async fn structural_atlas(
        &self,
        corpus_id: &str,
        indexes_dir: &Path,
        recipes_dir: &Path,
    ) -> Result<(serde_json::Value, serde_json::Value), String> {
        self.record("structural_atlas");
        match &self.structural_atlas {
            Some(f) => f(corpus_id, indexes_dir, recipes_dir),
            None => Err(unprogrammed("structural_atlas")),
        }
    }

    fn vital_tier(&self, canonical_name: &str) -> Option<u8> {
        self.record("vital_tier");
        match &self.vital_tier {
            Some(f) => f(canonical_name),
            None => panic!("{}", unprogrammed("vital_tier")),
        }
    }

    fn normalize_title(&self, title: &str) -> String {
        self.record("normalize_title");
        match &self.normalize_title {
            Some(f) => f(title),
            None => panic!("{}", unprogrammed("normalize_title")),
        }
    }

    fn pipeline_navigation(&self, pipeline_id: &str) -> Option<(String, NavigationPolicy)> {
        self.record("pipeline_navigation");
        match &self.pipeline_navigation {
            Some(f) => f(pipeline_id),
            None => panic!("{}", unprogrammed("pipeline_navigation")),
        }
    }

    fn argumentative_system(&self) -> &'static str {
        self.record("argumentative_system");
        match self.argumentative_system {
            Some(s) => s,
            None => panic!("{}", unprogrammed("argumentative_system")),
        }
    }

    fn argumentative_schema(&self) -> serde_json::Value {
        self.record("argumentative_schema");
        match &self.argumentative_schema {
            Some(f) => f(),
            None => panic!("{}", unprogrammed("argumentative_schema")),
        }
    }

    fn render_source_recovery_block(&self, excerpts: &[&str]) -> String {
        self.record("render_source_recovery_block");
        match &self.render_source_recovery_block {
            Some(f) => f(excerpts),
            None => panic!("{}", unprogrammed("render_source_recovery_block")),
        }
    }

    fn argumentative_atom_count(
        &self,
        response_text: &str,
        cross_leaf_only: bool,
    ) -> Result<usize, String> {
        self.record("argumentative_atom_count");
        match &self.argumentative_atom_count {
            Some(f) => f(response_text, cross_leaf_only),
            None => Err(unprogrammed("argumentative_atom_count")),
        }
    }

    fn write_typed_extension(
        &self,
        corpus_id: &str,
        atlas_dir: &Path,
        responses: &[ArgumentativeResponse],
        person_seeds: Vec<Entity>,
        citations: &HashMap<String, SourceCitation>,
        _embed_query: EmbedFn,
    ) -> corpus_index::Result<HashMap<String, u32>> {
        self.record("write_typed_extension");
        match &self.write_typed_extension {
            Some(f) => f(corpus_id, atlas_dir, responses, person_seeds, citations),
            None => Err(corpus_index::Error::Io(io::Error::other(unprogrammed(
                "write_typed_extension",
            )))),
        }
    }

    fn rank_starter_questions(&self, atoms: &[AtomEnvelope], limit: usize) -> Vec<StarterQuestion> {
        self.record("rank_starter_questions");
        match &self.rank_starter_questions {
            Some(f) => f(atoms, limit),
            None => panic!("{}", unprogrammed("rank_starter_questions")),
        }
    }

    fn detect_atom_spans(
        &self,
        text: &str,
        section_id: Option<&str>,
        atoms: &[AtomEnvelope],
    ) -> Vec<AtomSpan> {
        self.record("detect_atom_spans");
        match &self.detect_atom_spans {
            Some(f) => f(text, section_id, atoms),
            None => panic!("{}", unprogrammed("detect_atom_spans")),
        }
    }

    fn migrate_atlas_ids(
        &self,
        atlas_dir: &Path,
        corpus_id: &str,
        dry_run: bool,
    ) -> Result<String, String> {
        self.record("migrate_atlas_ids");
        match &self.migrate_atlas_ids {
            Some(f) => f(atlas_dir, corpus_id, dry_run),
            None => Err(unprogrammed("migrate_atlas_ids")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unprogrammed_result_method_errs_naming_itself() {
        let d = AtlasPortDouble::new();
        let err = d.atlas_summary(Path::new("/nowhere")).unwrap_err();
        assert!(
            err.to_string().contains("AtlasPortDouble::atlas_summary"),
            "{err}"
        );
        assert_eq!(d.calls(), vec!["atlas_summary"]);
    }

    #[test]
    #[should_panic(expected = "AtlasPortDouble::vital_tier: not programmed")]
    fn an_unprogrammed_plain_method_panics_naming_itself() {
        AtlasPortDouble::new().vital_tier("Earth");
    }

    #[test]
    fn a_programmed_method_answers_through_its_handler() {
        let d = AtlasPortDouble::new().on_vital_tier(|name| (name == "Earth").then_some(2));
        assert_eq!(d.vital_tier("Earth"), Some(2));
        assert_eq!(d.vital_tier("Mars"), None);
    }
}
