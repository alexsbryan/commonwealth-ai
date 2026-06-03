//! Atlas Open/Closed surface.
//!
//! The v2.1 enrichment architecture separates the stable atlas output
//! format (atoms, edges, brief assembler, schema validation) from the
//! ingestion strategies that populate it. This module owns the
//! trait and registry that let new strategies land without touching
//! the traversal engine or the downstream consumers of atlas data.
//!
//! # Current scope
//!
//! - `ingestion::AtlasIngestion` — the trait every ingestion strategy
//!   implements. One method: `ingest(corpus, embed_fn, inference_fn,
//!   config, progress) -> AtlasData`.
//! - `ingestion::AtlasData` — the bundle returned by `ingest`. Atoms,
//!   edges, trajectory index, manifest.
//! - `registry::AtlasIngestionRegistry` — string-id dispatch per
//!   ARCH_PRINCIPLES §4. Initially carries one entry:
//!   `extraction_first`, which wraps the existing 8-phase runner via
//!   an adapter.
//!
//! Further atlas-specific modules (`atoms`, `edges`, `resolution`,
//! `analysis/{tensions,gaps,configuration}`, `manifest`,
//! `cross_corpus`) land as the rollout progresses. They live under
//! this same namespace so the atlas surface is one importable module.

pub mod analysis;
pub mod atoms;
pub mod atoms_delta;
pub mod axis_catalog;
pub mod citation;
pub mod cross_corpus;
pub mod doc_to_atoms;
pub mod edges;
pub mod embeddings;
pub mod ingestion;
pub mod migrate_ids;
pub mod registry;
pub mod resolution;
pub mod schema_validation;
pub mod section_cache;
pub mod strategies;
pub mod summary;
pub mod vital_tier;
pub mod writer;

pub use atoms::{
    AtomEnvelope, AtomId, AtomType, AtomsFile, ChunkRef, Claim, Configuration, Entity, Event,
    Question, Relation, ResolutionStatus, SectionPosition, SectionRange, State,
};
pub use axis_catalog::{
    all_axes, axes_for_mode, axis_by_key, AtomKind, GatingField, TypedAxis, AXIS_CATALOG,
};
pub use citation::{apply_citation, SourceCitation};
pub use cross_corpus::{
    detect_grounding, CrossCorpusEdge, CrossCorpusEdgesFile, CrossCorpusInput, CrossCorpusReport,
    DetectorSummary, MatchTrace, PeerAtomRef, RejectionBucket, RejectionSample,
};
pub use edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
pub use embeddings::{
    atoms_content_hash, read_atlas_embeddings, write_atlas_embeddings, CachedAtlasEntry,
};
pub use ingestion::{AtlasData, AtlasIngestion, AtlasIngestionConfig};
pub use registry::AtlasIngestionRegistry;
pub use resolution::{
    fold, resolve_entities_and_events, resolve_step_3b, ResolutionOutput, Step3bOutput, Trajectory,
    TrajectoryState, TrajectoryTransition,
};
pub use schema_validation::{
    build_report as build_schema_validation_report, compare_across_corpora, count_open_questions,
    count_transitions_without_trigger, count_ungrounded_claims, SchemaComparison,
    SchemaValidationInput, SchemaValidationReport,
};
pub use summary::{
    compute_summary as compute_atlas_summary,
    read_or_compute_summary as read_or_compute_atlas_summary, AtlasSummary,
};
pub use vital_tier::{tier_sizes as vital_tier_sizes, vital_tier};
pub use writer::{
    read_atlas_atoms, read_atlas_cross_corpus_edges, read_atlas_edges, read_tension_candidates,
    write_atlas, write_atlas_configurations, write_atlas_cross_corpus_edges, write_atlas_edges,
    write_atlas_failures, write_atlas_full, write_atlas_gaps, write_tension_candidates,
    AtlasWritten, ResolutionFailuresFile, TrajectoriesFile, ATLAS_DIRNAME,
};

use std::path::Path;

/// Folder-ingest v1 §3.3 — atomically remove a corpus's
/// `atlas/` directory under its index root. Used when the user
/// disables enrichment on a watched-folder corpus, or when a
/// failed build leaves partial state behind.
///
/// "Atomic" here means: rename the live directory to a `.retired-
/// <ts>` sibling first, then `remove_dir_all` the renamed copy.
/// A concurrent reader that resolved the path before the rename
/// still sees a consistent atlas state (under the renamed path);
/// a reader that resolves after the rename sees no atlas dir.
/// The actual remove is best-effort — if it fails (permissions,
/// race, etc.), the renamed directory is left on disk for the
/// operator to inspect; subsequent calls re-rename to a fresh
/// timestamp so we never block on stale debris.
///
/// Idempotent: a missing `atlas/` dir returns `Ok(())` rather
/// than `Err`. Callers can drive teardown without first checking
/// existence.
///
/// # Examples
/// ```
/// # use std::fs;
/// # let dir = tempfile::tempdir().unwrap();
/// // No-op when the dir doesn't exist.
/// corpus_engine::atlas_teardown(dir.path(), "missing-corpus").unwrap();
/// ```
pub fn atlas_teardown(index_dir: &Path, corpus_id: &str) -> std::io::Result<()> {
    let atlas_dir = index_dir.join(corpus_id).join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        return Ok(());
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let retired = index_dir
        .join(corpus_id)
        .join(format!(".atlas-retired-{ts}"));
    std::fs::rename(&atlas_dir, &retired)?;
    // Best-effort delete. Failure here is logged, not fatal —
    // the rename already succeeded so the corpus is in the
    // post-teardown state from any reader's perspective.
    if let Err(e) = std::fs::remove_dir_all(&retired) {
        tracing::warn!(
            retired = %retired.display(),
            "atlas_teardown: rename succeeded but remove_dir_all failed: {e}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod teardown_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn teardown_missing_dir_is_ok() {
        // Idempotent: calling teardown on a corpus that has no
        // atlas/ subdir is a no-op success.
        let dir = tempdir().unwrap();
        atlas_teardown(dir.path(), "no-such-corpus").unwrap();
    }

    #[test]
    fn teardown_removes_atlas_directory() {
        let index = tempdir().unwrap();
        let atlas = index.path().join("c1").join(ATLAS_DIRNAME);
        fs::create_dir_all(&atlas).unwrap();
        fs::write(atlas.join("atoms.json"), "[]").unwrap();
        fs::write(atlas.join("edges.json"), "[]").unwrap();

        atlas_teardown(index.path(), "c1").unwrap();

        assert!(!atlas.exists(), "atlas dir should be gone");
        // The corpus's index dir survives — only `atlas/` was
        // removed. Other corpus state (chunks, manifest) lives
        // alongside and stays untouched.
        assert!(index.path().join("c1").exists());
    }

    #[test]
    fn teardown_is_atomic_via_rename() {
        // The teardown renames the atlas dir to a `.atlas-
        // retired-<ts>` sibling before deletion. Even if the
        // remove fails (e.g. a file is in use on Windows), the
        // canonical `atlas/` path is gone — readers that
        // resolve the path post-rename see no atlas state. The
        // retired sibling may linger but is not the canonical
        // location any reader looks at.
        let index = tempdir().unwrap();
        let atlas = index.path().join("c1").join(ATLAS_DIRNAME);
        fs::create_dir_all(&atlas).unwrap();
        fs::write(atlas.join("atoms.json"), "[]").unwrap();

        atlas_teardown(index.path(), "c1").unwrap();
        assert!(!atlas.exists(), "canonical atlas dir gone after teardown");
    }

    #[test]
    fn teardown_can_be_called_twice() {
        // Calling teardown after a successful teardown is a
        // no-op (the dir is already gone). This matters because
        // the watched-folder manager calls teardown on disable;
        // a user who clicks Disable then Disable-again must not
        // see an error.
        let index = tempdir().unwrap();
        let atlas = index.path().join("c1").join(ATLAS_DIRNAME);
        fs::create_dir_all(&atlas).unwrap();
        atlas_teardown(index.path(), "c1").unwrap();
        atlas_teardown(index.path(), "c1").unwrap();
    }
}
