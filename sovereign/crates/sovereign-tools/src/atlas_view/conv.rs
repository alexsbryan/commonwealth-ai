//! Conv-corpus DTOs for the desktop Atlas surface.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md` §"Retrieval
//! surface — A1 conv corpora in Atlas index".
//!
//! These types coexist with the existing `AtlasCorpusSummary` /
//! `AtomSummary` shapes (which are atoms.json-backed). Conv corpora
//! never wrote atoms.json — their tiered enrichment lives in the
//! `conv_skeletons` / `conv_raptor_nodes` / `conv_motifs` SQLite
//! sidecar tables. So we expose a parallel set of DTOs rather than
//! cramming RAPTOR clusters into the atom-typed shape.
//!
//! The desktop Atlas index calls BOTH `atlas_list_corpora` (atoms.json
//! sources) and `atlas_list_conv_corpora` (these), then merges
//! client-side under their respective `display_category` groupings.
//! AtlasSurface routes conv-category corpora to a conv-specific
//! drill-in component instead of the atom browser.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One row in the Atlas index for a conv corpus. Counts come from
/// `conv_skeletons` state-bucketed via
/// `count_conv_skeletons_by_state`; `last_updated_unix` is the max
/// `updated_at` across the corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvCorpusSummary {
    pub corpus_id: String,
    pub display_name: String,
    /// Total conversations enriched (any state).
    pub conv_count: u64,
    /// Per-state counts. Keys are `ConvTieredState::as_str()` values:
    /// "Pending", "PartiallyReady", "MultiHopReady", "Ready", "Failed".
    /// `BTreeMap` for deterministic JSON ordering.
    pub state_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_icon: Option<String>,
}

/// One row in `AtlasConvCorpusView` — a conversation as the
/// atlas-level unit. Maps to a single `conv_skeletons` row plus a
/// pre-computed `top_entities` digest from the conv's RAPTOR nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvSummary {
    pub conv_uuid: String,
    pub title: String,
    /// "Pending" | "PartiallyReady" | "MultiHopReady" | "Ready" |
    /// "Failed" — verbatim from `conv_skeletons.state`.
    pub state: String,
    pub chunk_count: i64,
    /// Up to 6 highest-salience entities across all RAPTOR nodes for
    /// this conv (salience = sum of `cluster_coherence` over nodes
    /// containing the entity). Empty for Tiny synthetic convs.
    pub top_entities: Vec<String>,
    pub updated_at: i64,
    /// True when this conv has only a single RAPTOR node with empty
    /// primary_entities — i.e., it took the Tiny opt-2 path during
    /// enrichment. UI uses this to suppress "entities" affordances
    /// that would render empty.
    pub is_tiny: bool,
}

/// Paginated response from `atlas_list_conversations`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvListPage {
    pub conversations: Vec<ConvSummary>,
    pub total_matching: u64,
    /// Offset to pass back for the next page; `None` when exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u64>,
}

/// One node in a conv's RAPTOR tree, surfaced to the desktop's
/// ConvDetail view. Roots first when listed (sorted by level
/// descending in the backing read method).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvRaptorNodeView {
    pub node_id: String,
    pub level: u8,
    pub summary: String,
    pub primary_entities: Vec<String>,
    pub direct_member_chunk_ids: Vec<u64>,
    pub evidence_chunk_count: usize,
    pub cluster_coherence: f64,
    /// Synthetic Tiny placeholder (RAPTOR didn't actually run; node
    /// just carries the conv title as `summary` with empty entities).
    /// UI suppresses the entity row and shows a "no clusters" note.
    pub is_synthetic_tiny: bool,
}

/// Full conv detail — title + state + full RAPTOR tree. The
/// frontend chooses between a flat (≤2 levels) or hierarchical (>2)
/// rendering based on `max_level`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvDetailView {
    pub corpus_id: String,
    pub conv_uuid: String,
    pub title: String,
    pub state: String,
    pub chunk_count: i64,
    pub updated_at: i64,
    pub raptor_nodes: Vec<ConvRaptorNodeView>,
    pub max_level: u8,
}

/// One entity chip for the ConversationChunkRenderer surface (A2).
/// `salience` is sum of `cluster_coherence` across nodes containing
/// the entity; `occurrence_count` is the number of distinct RAPTOR
/// nodes mentioning it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvEntityChip {
    pub name: String,
    pub salience: f32,
    pub occurrence_count: u32,
}
