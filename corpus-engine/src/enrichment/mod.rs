//! Field model enrichment layer.
//!
//! Clusters chunk embeddings via HDBSCAN, extracts a field skeleton from
//! overview passages using domain-specific prompts, aligns clusters to
//! named positions, detects fault lines and open questions.

pub mod alignment;
pub mod checkpoint;
pub mod clustering;
pub mod domain;
pub mod domains;
pub mod fault_lines;
pub mod field_engine;
pub mod filter;
pub mod open_questions;
pub mod skeleton;

pub use filter::is_chunk_eligible;
pub use field_engine::{FieldModelEngine, reprocess_skeleton_failures};
pub use clustering::{EnrichmentProgress, FieldModelStats};
pub use domain::Domain;
pub use skeleton::FieldSkeleton;
