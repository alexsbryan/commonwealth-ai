// SPDX-License-Identifier: AGPL-3.0-or-later
//! Clustering knobs the surface sends; the clusterer itself stays in
//! `sovereign-tools`, which names corpus-engine's clustering stack.
//!
//! Moved down from `sovereign_tools::local_corpus::clusterer` at svt-6
//! (2026-09-12) and re-exported there at the historical path. Pure serde over
//! primitives: a client that only wants to SPELL one of these had to link
//! `sovereign-tools` — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more. See this module's parent for the full note.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// HDBSCAN minimum cluster size. Smaller = more, tighter clusters.
    pub min_cluster_size: usize,
    /// Minimum cluster-assignment confidence a note must clear to be
    /// tagged. Notes below this threshold land in the outlier panel
    /// rather than being force-assigned to the nearest cluster.
    pub min_confidence: f32,
    /// Notes matching multiple clusters above this confidence are
    /// candidates for multi-tagging. v1 implements the `Dominant`
    /// strategy regardless; this field is wired for v2.
    pub multi_tag_threshold: f32,
    pub multi_cluster_strategy: MultiClusterStrategy,
    /// Minimum **distinct notes** per cluster after the chunk-to-note
    /// rollup. Clusters with fewer notes than this threshold are
    /// collapsed: their notes land in the outlier panel with reason
    /// `SingletonCluster`, and the cluster itself disappears from the
    /// preview. `#[serde(default)]` so callers written before this
    /// field existed still deserialise cleanly.
    #[serde(default = "default_min_notes_per_cluster")]
    pub min_notes_per_cluster: usize,
}

fn default_min_notes_per_cluster() -> usize {
    2
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MultiClusterStrategy {
    /// Tag only the highest-confidence cluster. v1 default.
    Dominant,
    /// Tag every cluster whose confidence exceeds `multi_tag_threshold`.
    All,
    /// Flag for manual review, no auto-tag.
    Flag,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 5,
            min_confidence: 0.4,
            multi_tag_threshold: 0.6,
            multi_cluster_strategy: MultiClusterStrategy::Dominant,
            min_notes_per_cluster: default_min_notes_per_cluster(),
        }
    }
}

/// Per-cluster label produced by the LLM. Structure mirrors spec §6.3
/// exactly: tag path (`domain/subtopic`), a display name for the UI,
/// and a 2–3 sentence description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledCluster {
    pub id: i32,
    pub tag_path: String,
    pub display_name: String,
    pub description: String,
    pub note_count: usize,
    /// Chunk IDs closest to the cluster centroid. Used to render the
    /// "representative notes" list in the review UI.
    pub centroid_chunk_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenQuestion {
    pub gap_description: String,
    pub relevant_cluster_ids: Vec<i32>,
}
