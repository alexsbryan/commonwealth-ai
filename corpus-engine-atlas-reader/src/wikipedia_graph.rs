//! The wiki link-graph query surface: `Neighbor` / `ArticleRecord` /
//! `WikipediaGraphApi`.
//!
//! Split from its one lancedb implementor (`corpus_engine::wikipedia_columnar::
//! ColumnarWikipediaGraph`, which imports the `wiki_store` writer) so svrn can
//! hold `Arc<dyn WikipediaGraphApi>` without linking corpus-engine (FIVE_PROGRAMS
//! §12 decision 1). corpus-engine re-exports all three at
//! `wikipedia_columnar::` (ARCH §10.6).

/// A one-hop neighbor in the link graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Neighbor {
    /// Target article title, canonical form (spaces, not underscores).
    pub title: String,
    /// Coarse relationship class derived from section + link text.
    /// One of `topical | causal | contested | defines | action | see-also`.
    /// Open text carried as DATA beside the closed `EdgeType` kind, never as a
    /// new enum arm (ARCH principle 9; EPISTEMIC_INDEX one-store-provider).
    pub relationship_type: String,
    /// How many sections of the source article link here. Higher =
    /// stronger structural signal, and the ranking key: the neighbor
    /// queries order by this summed across sections.
    pub occurrence_count: i64,
    /// True iff the target is itself an in-scope article (it has an
    /// `articles.lance` row). Dangling link targets are `false`.
    pub in_scope: bool,
}

/// A single article record. Exposed so callers can read derived
/// signals (cluster_id, bridge_score, contested totals) without a
/// second round-trip when both are needed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArticleRecord {
    pub title: String,
    pub wikidata_qid: Option<String>,
    pub revision_id: Option<i64>,
    pub in_scope: bool,
    /// Layer-1 slot (HDBSCAN cluster). Not written yet — always `None`.
    pub cluster_id: Option<i64>,
    /// Layer-1 slot (bridge detection). Not written yet — always `None`.
    pub bridge_score: Option<f64>,
    pub pov_total: i64,
    pub citation_total: i64,
}

/// The query surface the runtime consumes from a Wikipedia link graph.
/// `#[async_trait]` keeps it `dyn`-safe (the methods are async) so the runtime
/// holds `Arc<dyn WikipediaGraphApi>` (`LaneSources::wikipedia_graph`).
///
/// One implementor since W4: `corpus_engine::wikipedia_columnar::
/// ColumnarWikipediaGraph`. The trait stays because the runtime, the bridge
/// builder and the CLI all hold the graph behind it, and because the
/// one-store-provider work gives wiki-class a second face (`AtlasProvider`)
/// over the same two tables.
#[async_trait::async_trait]
pub trait WikipediaGraphApi: Send + Sync {
    async fn neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor>;
    async fn neighbors_for_axis(
        &self,
        title: &str,
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor>;
    async fn co_neighbors(
        &self,
        titles: &[String],
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor>;
    async fn reverse_neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor>;
    async fn has_contested_section(&self, title: &str) -> bool;
    async fn record(&self, title: &str) -> Option<ArticleRecord>;
    async fn article_count(&self) -> usize;
    async fn edge_count(&self) -> usize;
}
