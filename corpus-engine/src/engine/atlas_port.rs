// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's implementation of the atlas family's port
//! (`corpus_engine_atlas_reader::ports::AtlasPort`, pb-ingest-dial-tools-atlas).
//!
//! Every method is addressed by path and reads no engine state, so the
//! implementor is a unit value: a composition root, or a test, holds
//! `Arc::new(IngestAtlas)` without assembling an engine.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use corpus_engine_atlas_reader::citation::SourceCitation;
use corpus_engine_atlas_reader::ports::{ArgumentativeResponse, AtlasPort, AtomSpan};
use corpus_engine_atlas_reader::raptor_read::RaptorSummaryRow;
use corpus_engine_atlas_reader::summary::AtlasSummary;
use sovereign_contracts::daemon_wire::enrich::StarterQuestion;
use understanding_vocab::atoms::{AtomEnvelope, AtomsFile, Entity};
use understanding_vocab::edges::{Edge, EdgesFile};

use crate::enrichment::atlas::analysis::gaps::{
    detect_deterministic_gaps, GapDetectionInput, GapsOutput,
};
use crate::enrichment::atlas::analysis::tensions::{
    drop_same_named_speaker_pairs, select_candidates, CandidateSelectionInput,
    TensionCandidatesOutput,
};

/// Ingest's atlas: the one [`AtlasPort`] implementor.
#[derive(Debug, Clone, Copy, Default)]
pub struct IngestAtlas;

#[async_trait]
impl AtlasPort for IngestAtlas {
    fn atlas_summary(&self, atlas_dir: &Path) -> io::Result<Option<AtlasSummary>> {
        crate::enrichment::atlas::read_or_compute_atlas_summary(atlas_dir)
    }

    fn ann_table_is_fresh(&self, atlas_dir: &Path) -> bool {
        crate::enrichment::atlas::ann_store::ann_table_is_fresh(atlas_dir)
    }

    fn write_deterministic_gaps(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
        edges: &[Edge],
    ) -> io::Result<(usize, PathBuf)> {
        // Partition atoms by kind — only Claim / State / Question drive the
        // deterministic detectors; the rest pass through untouched.
        let mut claims = Vec::new();
        let mut states = Vec::new();
        let mut questions = Vec::new();
        for a in atoms.iter().cloned() {
            match a {
                AtomEnvelope::Claim(c) => claims.push(c),
                AtomEnvelope::State(s) => states.push(s),
                AtomEnvelope::Question(q) => questions.push(q),
                _ => {}
            }
        }

        let gaps = detect_deterministic_gaps(GapDetectionInput {
            claims: &claims,
            states: &states,
            questions: &questions,
            edges,
        });
        let n = gaps.len();
        let path = crate::enrichment::atlas::write_atlas_gaps(atlas_dir, &GapsOutput::new(gaps))?;
        Ok((n, path))
    }

    fn write_tension_candidates(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
    ) -> io::Result<(usize, PathBuf)> {
        // Claim + State drive the entity-overlap signal; Entity atoms feed the
        // cross-position concept-overlap signal.
        let mut claims = Vec::new();
        let mut states = Vec::new();
        let mut entities = Vec::new();
        for a in atoms.iter().cloned() {
            match a {
                AtomEnvelope::Claim(c) => claims.push(c),
                AtomEnvelope::State(s) => states.push(s),
                AtomEnvelope::Entity(e) => entities.push(e),
                _ => {}
            }
        }

        let mut candidates = select_candidates(CandidateSelectionInput {
            claims: &claims,
            states: &states,
            // Intra-cluster candidates aren't wired in the deterministic path
            // (same as the bespoke command — pending a stable sketch→atom map).
            claim_clusters: &[],
            entities: &entities,
        });
        // De-noise: drop pairs where both claims share a named speaker.
        drop_same_named_speaker_pairs(&mut candidates, &claims, &entities);

        let out = TensionCandidatesOutput::new(candidates);
        let n = out.candidates.len();
        let path = crate::enrichment::atlas::write_tension_candidates(atlas_dir, &out)?;
        Ok((n, path))
    }

    async fn build_raptor_index(
        &self,
        corpus_dir: &Path,
        rows: &[RaptorSummaryRow],
        source_version: i64,
    ) -> corpus_index::Result<usize> {
        crate::build_raptor_index(corpus_dir, rows, source_version).await
    }

    async fn scan_raptor_summaries(
        &self,
        corpus_dir: &Path,
    ) -> corpus_index::Result<Vec<RaptorSummaryRow>> {
        crate::scan_raptor_summaries(corpus_dir).await
    }

    fn raptor_article_title(&self, conv_uuid: &str) -> String {
        crate::raptor_article_title(conv_uuid)
    }

    fn write_atlas_edges(&self, atlas_dir: &Path, edges: &EdgesFile) -> io::Result<PathBuf> {
        crate::enrichment::atlas::write_atlas_edges(atlas_dir, edges)
    }

    fn write_atlas_atoms(&self, atlas_dir: &Path, atoms: &AtomsFile) -> io::Result<PathBuf> {
        crate::enrichment::atlas::write_atlas_atoms(atlas_dir, atoms)
    }

    fn write_population_marker(&self, atlas_dir: &Path) -> io::Result<()> {
        use crate::enrichment::atlas::seed_population::{seed_population, write_population_marker};
        write_population_marker(atlas_dir, &seed_population(atlas_dir))
    }

    async fn structural_atlas(
        &self,
        corpus_id: &str,
        indexes_dir: &Path,
        recipes_dir: &Path,
    ) -> Result<(serde_json::Value, serde_json::Value), String> {
        use crate::enrichment::atlas::{AtlasIngestionConfig, AtlasIngestionRegistry};
        use crate::ProgressCallback;
        use corpus_index::types::EmbedFn;
        use sovereign_contracts::daemon_wire::IngestProgress;
        use std::sync::Arc;

        let registry = AtlasIngestionRegistry::builtin();
        let Some(strategy) = registry.get("structure_first") else {
            return Err("structure_first strategy not registered".into());
        };

        // structure_first reads chunk metadata, never embeds — wire a
        // no-op EmbedFn so the engine constructor doesn't require a
        // model. Same pattern as the CLI's `enrich ingest` path.
        let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
        let engine = Arc::new(super::CorpusEngine::new(
            recipes_dir.to_path_buf(),
            indexes_dir.to_path_buf(),
            noop_embed.clone(),
        ));

        let cfg = AtlasIngestionConfig {
            strategy_id: "structure_first".into(),
            strategy_config: serde_json::json!({
                "source_corpus_id": corpus_id,
            }),
        };

        let progress: Arc<ProgressCallback> = Arc::new(Box::new(move |ev: IngestProgress| {
            tracing::debug!(?ev, "structural_atlas: progress");
        }));

        let data = strategy
            .ingest(engine, noop_embed, None, cfg, progress)
            .await
            .map_err(|e| format!("strategy.ingest failed: {e}"))?;
        Ok((data.atoms, data.edges))
    }

    fn vital_tier(&self, canonical_name: &str) -> Option<u8> {
        crate::enrichment::atlas::vital_tier(canonical_name)
    }

    fn normalize_title(&self, title: &str) -> String {
        crate::filters::normalize_title(title)
    }

    fn pipeline_navigation(
        &self,
        pipeline_id: &str,
    ) -> Option<(String, understanding_vocab::ontology::NavigationPolicy)> {
        crate::enrichment::pipeline::PipelineRegistry::builtin()
            .get(pipeline_id)
            .map(|p| (p.id().to_string(), p.declared_ontology().navigation))
    }

    fn argumentative_system(&self) -> &'static str {
        crate::enrichment::pipeline::typed_schemas::argumentative::PHASE1_ARGUMENTATIVE_SYSTEM
    }

    fn argumentative_schema(&self) -> serde_json::Value {
        crate::enrichment::pipeline::typed_schemas::argumentative::phase1_argumentative_schema()
    }

    fn render_source_recovery_block(&self, excerpts: &[&str]) -> String {
        crate::enrichment::pipeline::typed_schemas::render_source_recovery_block(excerpts)
    }

    fn argumentative_atom_count(
        &self,
        response_text: &str,
        cross_leaf_only: bool,
    ) -> Result<usize, String> {
        crate::enrichment::atlas::typed_extension::parse_argumentative(
            response_text,
            cross_leaf_only,
        )
        .map(|e| e.atom_count())
    }

    fn write_typed_extension(
        &self,
        corpus_id: &str,
        atlas_dir: &Path,
        responses: &[ArgumentativeResponse],
        person_seeds: Vec<Entity>,
        citations: &HashMap<String, SourceCitation>,
        embed_query: corpus_index::types::EmbedFn,
    ) -> corpus_index::Result<HashMap<String, u32>> {
        crate::enrichment::atlas::typed_extension::write_typed_extension(
            corpus_id,
            atlas_dir,
            responses,
            person_seeds,
            citations,
            embed_query,
        )
    }

    fn rank_starter_questions(&self, atoms: &[AtomEnvelope], limit: usize) -> Vec<StarterQuestion> {
        crate::enrichment::atlas::analysis::starter_questions::rank_starter_questions(atoms, limit)
    }

    fn detect_atom_spans(
        &self,
        text: &str,
        section_id: Option<&str>,
        atoms: &[AtomEnvelope],
    ) -> Vec<AtomSpan> {
        crate::atlas_traversal::detect_atom_spans(text, section_id, atoms)
    }

    fn migrate_atlas_ids(
        &self,
        atlas_dir: &Path,
        corpus_id: &str,
        dry_run: bool,
    ) -> Result<String, String> {
        crate::enrichment::atlas::migrate_ids::migrate_atlas_ids(atlas_dir, corpus_id, dry_run)
            .map(|summary| format!("{summary:?}"))
            .map_err(|e| e.to_string())
    }
}
