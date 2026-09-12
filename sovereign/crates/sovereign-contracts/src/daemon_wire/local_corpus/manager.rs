// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the local-corpus manager REPORTS: ingest statistics, the
//! resumable-job rows, and the progress-callback shape a surface installs.
//!
//! Moved down from `sovereign_tools::local_corpus::manager` at svt-6
//! (2026-09-12) and re-exported there at the historical path. Pure serde over
//! primitives: a client that only wants to SPELL one of these had to link
//! `sovereign-tools` — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more. See this module's parent for the full note.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::progress::{ExcerptChunk, LocalCorpusProgress, RuntimeFailure};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestStats {
    pub corpus_id: String,
    pub files_indexed: usize,
    pub chunks_written: u64,
    /// Files the pre-scan approved but that failed during the
    /// staging/extraction step. Named individually on the completion
    /// screen per spec §9.
    pub runtime_failures: Vec<RuntimeFailure>,
    /// Top 3 excerpts for the completion screen. Populated by M2
    /// (the excerpt scorer) — empty in M1.
    pub excerpt_chunks: Vec<ExcerptChunk>,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteJob {
    pub corpus_id: String,
    pub display_name: String,
    pub files_done: usize,
    pub files_total: usize,
}

/// Thread-safe progress callback — matches the shape the UI layer
/// already uses for public corpora.
pub type ProgressCallback = Arc<dyn Fn(LocalCorpusProgress) + Send + Sync>;
