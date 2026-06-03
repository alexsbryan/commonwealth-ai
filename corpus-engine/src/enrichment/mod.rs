//! Field model enrichment layer.
//!
//! Clusters chunk embeddings via HDBSCAN, extracts a field skeleton from
//! overview passages using domain-specific prompts, aligns clusters to
//! named positions, detects fault lines and open questions.

pub mod alignment;
pub mod atlas;
pub mod checkpoint;
pub mod clustering;
pub mod domain;
pub mod domain_registry;
pub mod domains;
pub mod entity_extraction;
pub mod fault_lines;
pub mod field_engine;
pub mod filter;
pub mod investigation;
pub mod open_questions;
pub mod pipeline;
pub mod reconciliation;
pub mod sep;
pub mod skeleton;
pub(crate) mod skeleton_parse;
pub mod state;
pub mod tiered;

pub use state::{
    sweep_stalled_states, CompositeSink, EnrichmentPhase, EnrichmentProgressSink, EnrichmentState,
    EnrichmentStateFile, StateFileSink, ENRICHMENT_STATE_FILENAME, STALL_THRESHOLD_SECS,
};

pub use atlas::{AtlasData, AtlasIngestion, AtlasIngestionConfig, AtlasIngestionRegistry};

pub use clustering::{EnrichmentProgress, FieldModelStats};
pub use domain::Domain;
pub use field_engine::{reprocess_skeleton_failures, FieldModelEngine};
pub use filter::is_chunk_eligible;
pub use skeleton::FieldSkeleton;

// v2 enrichment pipeline (coexists with v1 during iteration; see
// `pipeline::mod` for the migration plan).
pub use pipeline::{Pipeline, PipelineRegistry};
