// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire shapes of the enrichment-store reads — `GET /internal/corpus/enriched`
//! and `GET /internal/corpus/{corpus}/starter-questions`
//! (`sovereign_mesh::enrich_http`). Moved below the daemon 2026-09-11
//! (thin-desktop order): the desktop read the enrichment store's directory
//! tree and folded atoms itself to produce these, which meant a thin client
//! linked `sovereign-enrichment-catalog` and the knowledge engine to render
//! a list and a row of chips.

use serde::{Deserialize, Serialize};

/// One enrichment workspace, as the corpus list wants to show it.
///
/// `sovereign_enrichment_catalog::EnrichedCorpusSummary`, moved; that crate
/// re-exports it at the old path. `Deserialize` added so the wire parses
/// back — it does not change a byte of the serialised form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedCorpusSummary {
    pub corpus_id: String,
    pub pipeline_id: String,
    /// The configured source, rendered for display. `EnrichConfig::source_path`
    /// is a `PathBuf`; this is the string a UI puts on screen.
    pub source_path: String,
    pub created_at: String,
}

/// One starter question mined from a corpus's Question atoms by
/// `corpus_engine::enrichment::atlas::analysis::starter_questions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarterQuestion {
    /// The question text, normalised to end in `?`.
    pub text: String,
    /// The Question atom it came from.
    pub atom_id: String,
    /// The section (`raised_at[0].chunk_id`) it was raised in, when known.
    pub source_section: Option<String>,
    /// `thematic` | `interpretive` | `open` | `factual` | `rhetorical` | …
    pub question_type: String,
}
