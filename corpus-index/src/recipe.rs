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
