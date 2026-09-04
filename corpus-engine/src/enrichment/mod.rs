// SPDX-License-Identifier: AGPL-3.0-or-later
//! Field model enrichment layer.
//!
//! Clusters chunk embeddings via HDBSCAN, extracts a field skeleton from
//! overview passages using domain-specific prompts, aligns clusters to
//! named positions, detects fault lines and open questions.

pub mod alignment;
pub mod atlas;
pub mod checkpoint;
pub mod clustering;
pub mod code_intel;
pub mod domain;
pub mod domain_registry;
pub mod domains;
pub mod entity_extraction;
pub mod fault_lines;
pub mod field_engine;
pub mod filter;
pub mod governance;
pub mod governance_change;
pub mod governance_view;
pub mod investigation;
pub mod ontology;
pub mod open_questions;
pub mod pass;
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

// Event-sourced governance oplog + active-set fold (Governance Atlas).
pub use governance::{
    derive_active, first_unattended_act, ActiveSet, GovernanceOpKind, RuleStatus, TensionStatus,
};
// `OpId` is not governance's — it is the shared journal's, and every tenant of
// `crate::oplog` mints one. Re-exported here so the governance surface reads
// whole, but the definition has exactly one home.
pub use crate::oplog::OpId;
// Governance read-model — the atlas-graph + oplog join (Governance Atlas).
pub use governance_view::{
    build_view, GovernanceIssue, GovernanceView, RuleAtom, RuleTension, RuleView,
    TensionDisposition, TensionView,
};
