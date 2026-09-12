// SPDX-License-Identifier: AGPL-3.0-or-later
//! The vault preview a user approves before anything is written back.
//!
//! Moved down from `sovereign_tools::local_corpus::preview` at svt-6
//! (2026-09-12) and re-exported there at the historical path. Pure serde over
//! primitives: a client that only wants to SPELL one of these had to link
//! `sovereign-tools` — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more. See this module's parent for the full note.

use serde::{Deserialize, Serialize};

use super::clusterer::{LabeledCluster, OpenQuestion};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPreview {
    pub clusters: Vec<ClusterSummary>,
    pub outliers: Vec<OutlierNote>,
    /// Notes that would get flagged under the `Flag` multi-cluster
    /// strategy. Empty for v1 (`Dominant` is always active).
    pub flagged: Vec<FlaggedNote>,
    pub total_notes: usize,
    pub tagged_notes: usize,
    pub outlier_count: usize,
    pub open_questions: Vec<OpenQuestion>,
    /// The namespace every `primary_tag` / `additional_tag` is
    /// prefixed with. Always `"sovereign"` for v1; here so the UI
    /// can render it without hardcoding.
    pub namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSummary {
    pub cluster: LabeledCluster,
    pub assignments: Vec<FileAssignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAssignment {
    pub chunk_id: u64,
    pub relative_path: String,
    pub note_title: String,
    pub primary_tag: String,
    pub additional_tags: Vec<String>,
    pub confidence: f32,
    pub existing_tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlierNote {
    pub chunk_id: u64,
    pub relative_path: String,
    pub note_title: String,
    pub best_cluster_id: i32,
    pub best_cluster_confidence: f32,
    pub reason: OutlierReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutlierReason {
    LowConfidence {
        threshold: f32,
    },
    AmbiguousCluster {
        top_clusters: Vec<ClusterConfidence>,
    },
    TooShort {
        char_count: usize,
    },
    /// The note's best-matching cluster ended up with fewer than
    /// `min_notes_per_cluster` notes after the chunk-to-note rollup,
    /// so we don't tag it on its own. `cluster_size` is how many
    /// notes were in that collapsed cluster.
    SingletonCluster {
        cluster_size: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfidence {
    pub cluster_id: i32,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaggedNote {
    pub chunk_id: u64,
    pub note_title: String,
    pub candidate_clusters: Vec<ClusterConfidence>,
}
