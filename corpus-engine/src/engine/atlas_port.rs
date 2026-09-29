// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's implementation of the atlas family's port
//! (`corpus_engine_atlas_reader::ports::AtlasPort`, pb-ingest-dial-tools-atlas).
//!
//! Every method is addressed by path and reads no engine state, so the
//! implementor is a unit value: a composition root, or a test, holds
//! `Arc::new(IngestAtlas)` without assembling an engine.

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use corpus_engine_atlas_reader::ports::AtlasPort;
use corpus_engine_atlas_reader::raptor_read::RaptorSummaryRow;
use corpus_engine_atlas_reader::summary::AtlasSummary;
use understanding_vocab::atoms::AtomEnvelope;
use understanding_vocab::edges::Edge;

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
}
