// SPDX-License-Identifier: AGPL-3.0-or-later
//! The columnar wiki store's FORMAT: its version, its table and directory
//! names, and the two row types. The read half of corpus-engine's
//! `enrichment::atlas::wiki_store`, which keeps the writer and re-exports
//! these at their historical paths (phase-b pb-corpus-mcp-reads).

/// Wiki columnar store schema version. Bump on any `articles.lance` /
/// `edges.lance` column change (e.g. the Layer-1 cluster/bridge columns or a
/// future article-embedding column).
///
/// **v2 (2026-09-04)** added `atom_id` + `chunk_id` to `articles.lance`. Without
/// them the store cannot answer [`crate::provider::AtlasProvider`]: the walk
/// keys on an atom id where the neighbor API keys on a title, and
/// `atom_evidence` has no chunk to hand back. A v1 store has neither column and
/// must be rebuilt (`svrn atlas wikipedia build-graph`).
pub const WIKI_STORE_FORMAT_VERSION: u32 = 2;

/// The columnar article store directory name (a Lance table under `atlas/`).
pub const ARTICLES_LANCE_DIRNAME: &str = "articles.lance";
/// The columnar edge store directory name (a Lance table under `atlas/`).
pub const EDGES_LANCE_DIRNAME: &str = "edges.lance";
pub const ARTICLES_TABLE: &str = "articles";
pub const EDGES_TABLE: &str = "edges";

/// One wiki article as v2 columns — the structural fields the `WikipediaGraph`
/// `record` / `has_contested_section` surface reads. Keyed by `title`. Nullable
/// source fields collapse to sentinels (`""` qid, `-1` revision), matching the
/// SEP store convention.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiArticleRow {
    /// Content-hash atom id — [`wiki_atom_id`]. The walk's key, and the seed
    /// table's. Distinct from `title`, which is the link graph's key: the two
    /// faces of this store address the same article by different handles
    /// because their questions differ, and both are stored so neither has to
    /// derive the other's.
    pub atom_id: String,
    /// Canonical article title (the neighbor-API + link-graph key).
    pub title: String,
    /// Wikidata QID (`""` if absent).
    pub wikidata_qid: String,
    /// Revision id for the freshness gate (`-1` if absent).
    pub revision_id: i64,
    /// False for a dangling link target not itself in indexed scope.
    pub in_scope: bool,
    /// Aggregate POV-flag count across the article's sections (contested signal).
    pub pov_total: i64,
    /// Aggregate citation-needed count (sourcing signal).
    pub citation_total: i64,
    /// Any section flagged contested (`pov_count > 0` OR a `controversy`
    /// section) — the `has_contested_section` signal, denormalised to the
    /// article so the check is a single column read.
    pub is_contested: bool,
    /// The chunk this article's atom cites — its evidence anchor, and the
    /// join key to `chunks.lance` for the migrated seed table. The LOWEST
    /// chunk id among the article's chunks: deterministic across rebuilds
    /// (a `HashMap` iteration order is not), and the lead section, since the
    /// chunker emits in document order.
    pub chunk_id: String,
}

/// One link-graph edge as v2 columns — row per
/// `(source_title, source_section_path, target_title)`, mirroring the SQLite
/// `edges` table. `occurrence_count` is per-section (the neighbor query SUMs
/// across sections); `source_section_path` drives `neighbors_for_axis` filtering;
/// `target_in_scope` is denormalised so the neighbor query is a single-table
/// predicate scan.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiEdgeRow {
    pub source_title: String,
    pub target_title: String,
    /// `topical | causal | contested | defines | action | see-also`
    /// (the `classify_relationship` axis).
    pub relationship_type: String,
    /// The link's anchor text — one of the three fields `neighbors_for_axis`
    /// matches axis terms against (with `target_title` + `source_section_path`).
    pub link_text: String,
    pub occurrence_count: i64,
    /// `›`-joined section path the link sits in (axis-filter key).
    pub source_section_path: String,
    /// Whether `target_title` is itself an in-scope article.
    pub target_in_scope: bool,
}
