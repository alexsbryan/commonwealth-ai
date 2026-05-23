//! Conversation tiered-retrieval types + read trait.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md`.
//!
//! Lives in `sovereign-core` (not `sovereign-store`) so the briefing
//! builder in [`crate::conv_briefing`] and the `Runtime` field can
//! reference the trait without `sovereign-core → sovereign-store →
//! sovereign-core` cyclic dependency. The concrete impl on
//! `SqliteStateStore` lives in `sovereign-store::sqlite` per the
//! existing trait/impl split for `StateStore`.

/// State variant for one conversation's tiered enrichment progress.
/// Mirrors `AssetState` but unblocks string-storage of the tag so
/// the briefing layer can render bare strings without serde dance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvTieredState {
    Pending,
    PartiallyReady,
    MultiHopReady,
    Ready,
    Failed,
}

impl ConvTieredState {
    pub fn as_str(self) -> &'static str {
        match self {
            ConvTieredState::Pending => "Pending",
            ConvTieredState::PartiallyReady => "PartiallyReady",
            ConvTieredState::MultiHopReady => "MultiHopReady",
            ConvTieredState::Ready => "Ready",
            ConvTieredState::Failed => "Failed",
        }
    }
}

/// One row from `conv_skeletons` — per-conversation enrichment state
/// + T2 partial skeleton + T3 overview/segments.
#[derive(Debug, Clone)]
pub struct ConvSkeletonRow {
    pub corpus_id: String,
    pub conv_uuid: String,
    pub state: String,
    pub skeleton_json: Option<String>,
    pub overview: Option<String>,
    pub segments_json: Option<String>,
    pub chunk_count: i64,
    pub updated_at: i64,
}

/// One row from `conv_raptor_nodes` — corpus-namespaced RAPTOR tree
/// node. Pre-serialised JSON blobs match the column types.
#[derive(Debug, Clone)]
pub struct ConvRaptorNodeRow {
    pub node_id: String,
    pub corpus_id: String,
    pub conv_uuid: String,
    pub level: i64,
    pub summary: String,
    pub summary_embedding: Vec<f32>,
    pub centroid_embedding: Vec<f32>,
    pub children_node_ids_json: String,
    pub direct_member_chunk_ids_json: Option<String>,
    pub evidence_chunk_ids_json: String,
    pub quote_spans_json: String,
    pub primary_entities_json: String,
    pub cluster_coherence: f64,
    pub created_at: i64,
}

/// One row from `conv_motifs` — TF-IDF distinctive term per conv.
#[derive(Debug, Clone)]
pub struct ConvMotifRow {
    pub corpus_id: String,
    pub conv_uuid: String,
    pub term: String,
    pub tf_idf_score: f64,
    pub occurrence_chunk_ids_json: String,
    pub is_distinctive: bool,
}

/// Read-side handle the briefing builder calls to surface
/// conv-tiered enrichment in retrieval prompts. The concrete impl on
/// `SqliteStateStore` ships in `sovereign-store::sqlite`.
///
/// Future ports (vault, SEP, corpus-wide RAPTOR) either impl this
/// trait directly OR motivate consolidation into a generic
/// `TieredRetrievalSurface` per the spec's §"Retrieval surface —
/// next session's trait" planning section.
#[async_trait::async_trait]
pub trait ConvTieredReader: Send + Sync {
    async fn list_conv_skeletons_for_corpus(
        &self,
        corpus_id: &str,
        conv_uuids: &[String],
    ) -> crate::error::Result<Vec<ConvSkeletonRow>>;

    async fn list_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> crate::error::Result<Vec<ConvRaptorNodeRow>>;
}
