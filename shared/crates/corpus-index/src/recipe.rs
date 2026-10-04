// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe settings the index persists.
//!
//! Per DE "The read-port leaf, measured again" ("a type the index persists is
//! defined by the index"): `DisplayMeta` and `MutableMergePolicy` are written
//! into `_corpus_meta.json` by the index, so they are DEFINED here and the
//! recipe EMBEDS them — `corpus-engine`'s `recipe` module re-exports both at
//! their historical paths.

use serde::{Deserialize, Serialize};

/// Presentation hints for a recipe.
///
/// Pure UI metadata: the retrieval layer reads `category` to decide
/// whether to render a chunk under "From your conversations" rather
/// than the corpus_id slug (see `format_scored_chunks_with_kinds`),
/// and the Atlas View rail groups corpora that share a category under
/// one header. No semantic meaning is attached to category strings —
/// add new ones as needed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DisplayMeta {
    /// Logical group this corpus belongs to. Example values:
    /// `"conversation"`, `"reference"`, `"argument"`, `"personal"`.
    /// `None` means "ungrouped" — UI buckets these as "Other".
    pub category: Option<String>,
    /// Optional icon hint for desktop tiles. Free-form string; the
    /// frontend maps known values (`"chat-bubble"`, `"book"`, …) onto
    /// its icon set and falls back to a generic glyph for unknown
    /// values.
    pub icon: Option<String>,
}

/// Reconciliation policy invoked by `corpus-engine`'s `sharding::merge_shards`
/// when the merged target's `_corpus_meta.json` carries a
/// `mutable_merge` value. Default (`None`) preserves classic
/// content-hash dedupe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutableMergePolicy {
    /// Group rows by `source_doc_id`. When a logical key collides,
    /// keep the row with the highest `mtime`. Rows whose
    /// `source_doc_id` is null fall back to content-hash dedupe.
    SourceDocIdNewestMtime,
}

/// Pairs with `CorpusMeta::kind = Catalog`. Tells the on-demand
/// ingest service how to take a catalog entry and produce a fully
/// ingested per-work corpus from it. See `gutenberg/recipe.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogConfig {
    /// Field name on the catalog `ExtractedDoc` (or its metadata
    /// blob) that uniquely identifies a work. Used by the on-demand
    /// flow to substitute into `download_url_template` and to derive
    /// the per-work corpus id (`<catalog_id>-<work_id>`).
    pub id_field: String,

    /// URL template with a `{id}` placeholder, e.g.
    /// `"https://www.gutenberg.org/cache/epub/{id}/pg{id}.txt"`.
    /// Resolved at on-demand ingest time and injected as the sole
    /// `[acquire] url` of the content recipe.
    pub download_url_template: String,

    /// Recipe id of the content recipe used to perform the
    /// per-work ingest, e.g. `"gutenberg-work"`. Must be `on_demand =
    /// true` and live in the registry.
    pub content_recipe: String,

    /// Optional name of a metadata column carrying an estimated
    /// word count (used to compute an ingest-time estimate the UI
    /// can show).
    #[serde(default)]
    pub estimated_words_field: Option<String>,

    /// Throughput estimate for the ingest stage, in words per
    /// minute. Combined with `estimated_words` to produce the
    /// "this will take ~N minutes" surface. Default 8000 wpm
    /// (conservative for an M-class machine on the embed slot).
    #[serde(default)]
    pub ingest_estimate_wpm: Option<u32>,

    /// Throughput estimate for the enrichment stage, in words per
    /// minute. Default 500 wpm.
    #[serde(default)]
    pub enrich_estimate_wpm: Option<u32>,

    /// Optional shared corpus id that catalog-driven ingests append
    /// into. When set, every successful work-ingest writes its
    /// chunks into a single growing corpus (e.g. `"wikipedia-fetched"`)
    /// instead of creating one corpus per work. Atlas, mesh-share,
    /// and retrieval all happen against the single shared corpus —
    /// a much better fit for catalogs whose long-tail can be
    /// thousands of articles. When unset (default), the legacy
    /// per-work pattern (`<catalog_id>-<work_id>`) is used.
    #[serde(default)]
    pub target_corpus_id: Option<String>,

    /// Enable one-hop "minesweeper" link-expansion after fetching an
    /// article. When true, the just-ingested article's outgoing
    /// links are queued for follow-up fetch into the same
    /// `target_corpus_id`. Only meaningful when `target_corpus_id`
    /// is set — without a shared target each expansion would
    /// spawn yet another per-work corpus.
    #[serde(default)]
    pub expansion_enabled: bool,

    /// Maximum number of linked articles to fetch in expansion.
    /// Ranking is significance-first (lead-section links beat
    /// body-section links, then document order). Default 20 keeps
    /// the per-fetch cost bounded; raise for deeper neighbourhood
    /// pre-loading, lower for fastest-only-the-asked behaviour.
    #[serde(default = "default_expansion_link_cap")]
    pub expansion_link_cap: u32,
}

fn default_expansion_link_cap() -> u32 {
    20
}
