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
pub mod cross_corpus;
pub mod edges;
pub mod ingestion;
pub mod registry;
pub mod resolution;
pub mod schema_validation;
pub mod writer;

pub use atoms::{
    AtomEnvelope, AtomId, AtomType, AtomsFile, ChunkRef, Claim, Configuration, Entity, Event,
    Question, Relation, ResolutionStatus, SectionPosition, SectionRange, State,
};
pub use edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
pub use ingestion::{AtlasData, AtlasIngestion, AtlasIngestionConfig};
pub use registry::AtlasIngestionRegistry;
pub use resolution::{
    fold, resolve_entities_and_events, resolve_step_3b, ResolutionOutput, Step3bOutput,
    Trajectory, TrajectoryState, TrajectoryTransition,
};
pub use cross_corpus::{
    detect_grounding, CrossCorpusEdge, CrossCorpusEdgesFile, CrossCorpusInput,
    CrossCorpusReport, DetectorSummary, MatchTrace, PeerAtomRef, RejectionBucket,
    RejectionSample,
};
pub use schema_validation::{
    build_report as build_schema_validation_report, compare_across_corpora,
    count_open_questions, count_transitions_without_trigger, count_ungrounded_claims,
    SchemaComparison, SchemaValidationInput, SchemaValidationReport,
};
pub use writer::{
    read_atlas_atoms, read_atlas_cross_corpus_edges, read_atlas_edges, write_atlas,
    write_atlas_configurations, write_atlas_cross_corpus_edges, write_atlas_failures,
    write_atlas_full, write_atlas_gaps, write_tension_candidates, AtlasWritten,
    ResolutionFailuresFile, TrajectoriesFile, ATLAS_DIRNAME,
};
