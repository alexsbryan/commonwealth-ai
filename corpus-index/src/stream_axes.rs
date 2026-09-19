// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-corpus stream metadata the index persists.
//!
//! `Stability`, `StreamAxes` and `StreamAxesSource` are written into
//! `_corpus_meta.json` at install time, so they are DEFINED here and embedded
//! by the recipe (DE "The read-port leaf, measured again": "the per-corpus
//! metadata goes to the leaf beside `IndexMeta`"). The stability *derivation*
//! (`derive_stability`) stays with Ingest, which reads the recipe's acquirer
//! and update blocks.

use serde::{Deserialize, Serialize};

/// Stability axis — what temporal contract the corpus carries.
///
/// Derived per-corpus at install time. See `derive_stability` in
/// `corpus-engine`'s `stream_axes` for the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    /// Snapshot release; re-ingest replaces wholesale. Wiki Core
    /// snapshot, SEP bulk, Gutenberg works, HuggingFace dataset
    /// dumps.
    Frozen,
    /// Active revision; expected to delta-ingest. Wiki Full
    /// expansions, U.S. Code (annual revisions), Federal Register,
    /// code corpora, watched folders.
    Versioned,
    /// Continuously updated within a window. Wiki Newsworthy,
    /// conversation history, codex telemetry.
    Rolling,
}

impl Stability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stability::Frozen => "frozen",
            Stability::Versioned => "versioned",
            Stability::Rolling => "rolling",
        }
    }
}

/// Persisted combined shape — written into `_corpus_meta.json` at
/// install time. The corpus's stream block; per-corpus stability
/// only. Articulation lives on each atom's anchor in the meta-atlas
/// (not here), so the per-corpus shape is just `{stability, source,
/// derived_at, from_signal}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamAxes {
    pub stability: Stability,
    /// Where the stability value came from: `"derived"` (from recipe
    /// fields), `"recipe_override"` (`[corpus.stream] stability = ...`
    /// in the recipe TOML), or `"backfill"` (Stage 2's
    /// `sovereign corpus stream-axes` filled in a legacy meta file).
    pub source: StreamAxesSource,
    /// Unix seconds at which the block was written. Matches the
    /// timestamp shape `IndexMeta::created_at` / `last_updated` use,
    /// so an operator can `date -r <derived_at>` to read it.
    pub derived_at: u64,
    /// Free-text summary of the recipe signal that drove the
    /// derivation. Empty when source is `recipe_override`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_signal: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamAxesSource {
    Derived,
    RecipeOverride,
    Backfill,
}
