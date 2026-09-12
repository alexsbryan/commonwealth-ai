// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's read of `GET /internal/corpus/local/{corpus}/ingest/progress`
//! (`sovereign_mesh::lc_http::IngestProgress`).
//!
//! NOT a relocation. `IngestProgress` closes over
//! `corpus_engine::enrichment::state::EnrichmentState` (the phase file),
//! which has no home at this layer. The route keeps the whole type; these are
//! the fields a client reads, deserialised from the
//! SAME bytes (serde ignores the rest). The receipt's COUNTS were a second
//! parameter until svt-6 (2026-09-12); `IngestStats` lives at this layer now
//! (`local_corpus::manager`), so the view names it. `sovereign-mesh`'s `wire_view_drift`
//! test serialises the real `IngestProgress` and parses this from it, so a
//! rename on the route side is red there rather than a silent `None` here
//! (ARCH principle 5: a check with a failing input you can name; principle
//! 6: absence is reported, never defaulted — every field below that the
//! route always writes is NOT `serde(default)`).

use serde::{Deserialize, Serialize};

use super::local_corpus::manager::IngestStats;

/// What `GET …/{corpus}/ingest/progress` answers, as a client reads it.
///
/// The `Stats` parameter is GONE (svt-6): the receipt's counts are
/// [`super::local_corpus::manager::IngestStats`], which lives at this layer
/// now, so the view names it instead of asking every client to choose between
/// linking `sovereign-tools` and passing `serde_json::Value` through.
#[derive(Debug, Serialize, Deserialize)]
pub struct IngestProgressView {
    /// The corpus being ingested.
    pub corpus_id: String,
    /// The live phase stamp, when one exists.
    pub state: Option<IngestPhaseView>,
    /// The terminal receipt, when the ingest half has ended.
    pub outcome: Option<IngestOutcomeView>,
    /// `true` iff `outcome` is present. Spelled out by the route rather
    /// than left to the caller so two clients cannot disagree about what
    /// terminal means.
    pub finished: bool,
}

/// The fields a client reads off the phase file
/// (`corpus_engine::enrichment::state::EnrichmentState`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestPhaseView {
    /// Progress within the current phase — `step_current` of `step_total`.
    pub step_current: u64,
    /// Steps in the current phase; `0` before the stamper knows.
    pub step_total: u64,
    /// The stamper's human-readable message for the step, when it set one.
    #[serde(default)]
    pub message: Option<String>,
}

/// The terminal receipt (`sovereign_mesh::corpus_watch_http::IngestOutcome`),
/// as a client reads it. Exactly one of `stats` / `error` is set by the
/// recorder; a receipt with neither is a host contradiction, and the
/// desktop says so rather than closing the panel on an invented success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestOutcomeView {
    /// The corpus the receipt is for.
    pub corpus_id: String,
    /// The job that produced it.
    pub job_id: String,
    /// Unix seconds when the ingest half finished, either way.
    pub finished_at: i64,
    /// `Some` on success — the counts, verbatim from the manager.
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub stats: Option<IngestStats>,
    /// `Some` on failure, naming it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ─── Corpus ingest — `GET /internal/corpus/progress` (commonwealth-api) ──

/// Progress updates during corpus ingestion — the engine's progress
/// callback payload AND the wire shape of `CorpusStatusEntry.progress` in
/// commonwealth-api's `GET /internal/corpus/progress`, externally tagged by
/// variant name.
///
/// Defined here since 2026-09-11 (thin-desktop order) because a client that
/// parses the daemon's progress feed had to link `corpus-engine` to name
/// it. `corpus_engine::IngestProgress` re-exports this, so the engine, its
/// `ProgressCallback` and commonwealth-api are unchanged; pure serde over
/// primitives, no engine type closes over it. Distinct from
/// `sovereign_mesh::lc_http::IngestProgress` (the local-corpus outcome
/// file), which is a different route's answer and keeps its own name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IngestProgress {
    Downloading {
        percent: f32,
        bytes_downloaded: u64,
        bytes_total: Option<u64>,
    },
    Extracting {
        documents_processed: u64,
    },
    Chunking {
        chunks_created: u64,
    },
    Embedding {
        chunks_embedded: u64,
        /// Total chunks expected. Zero means unknown (e.g. streaming extraction).
        total: u64,
        /// Number of source documents processed so far.
        docs_processed: u64,
        /// Embedding throughput in chunks per second over the last batch.
        chunks_per_sec: f32,
        /// Best-effort upper bound on the number of source documents the
        /// active filter expects to accept (e.g. ~51K for Wikipedia +
        /// `vital_articles_l5`). `None` for unfiltered ingests, where
        /// the natural denominator is shard-scan progress instead.
        ///
        /// When `Some`, the desktop UI prefers `docs_processed /
        /// expected_docs` over the shard-based estimate — that ratio is
        /// the only honest signal for filtered ingests, where the
        /// extractor must scan the entire source ZIP regardless of how
        /// few documents the filter accepts.
        expected_docs: Option<u64>,
    },
    Indexing {
        chunks_indexed: u64,
        total: u64,
    },
    /// Background IVF-PQ rebuild after a delta expansion. Surfaces as
    /// "Optimizing search index…" in the UI; search remains live
    /// throughout. Emitted by `CorpusEngine::expand_corpus` after the
    /// new vectors land — the centroids trained at the original (smaller)
    /// scope are suboptimal at the new scale, so the index is rebuilt
    /// in place.
    ///
    /// `current_chunks` is the chunk count at rebuild start (also the
    /// total since rebuilds run on a frozen snapshot — partitions are
    /// re-trained against the existing data, no chunks are added).
    OptimizingIndex {
        current_chunks: u64,
    },
    /// Post-embed enrichment phases (skeleton extraction, entity
    /// extraction, embedding clustering, cluster labeling, atlas
    /// build). Emitted by the field engine via the enrichment progress
    /// callback after the embed/index pipeline finishes; the desktop
    /// UI maps `phase` to a human-readable label and shows
    /// `detail` verbatim.
    ///
    /// Without this variant, the desktop polled `/internal/corpus/progress`
    /// and saw the last `Embedding` event from before enrichment
    /// started — so any ingest with enrichment enabled (conversations-
    /// anthropic, atlas-bearing recipes) appeared to hang at
    /// "Embedding chunks…" while clustering or entity extraction
    /// burned CPU silently. Observed 2026-05-20 mid-conversations
    /// ingest.
    ///
    /// `phase` is a stable machine token (`skeleton-extraction`,
    /// `entity-extraction`, `clustering`, `cluster-labeling`,
    /// `phase-skipped`, `resuming`). `detail` carries the same
    /// human-readable text the daemon already eprintln's. `fraction`
    /// is set when the underlying phase reports a numeric ratio
    /// (Phase 1b batch progress); `None` otherwise.
    Enriching {
        phase: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fraction: Option<f32>,
    },
    Complete {
        total_chunks: u64,
        duration_secs: u64,
    },
    /// Terminal failure: the ingest returned `Err` and committed
    /// nothing. Deliberately not emitted for
    /// `corpus_engine::Error::Cancelled` — a user-requested stop is not a
    /// failure, and the daemon clears that corpus's progress entry
    /// rather than recording one.
    ///
    /// This variant exists because the vocabulary could previously
    /// express success but not failure, so a failed ingest had nowhere
    /// to be recorded. `spawn_corpus_install` logged a `tracing::warn!`
    /// and removed the corpus from `active_ingests`; the corpus then
    /// vanished from `/internal/corpus/status`, and the Desktop
    /// poller's "entry disappeared from the snapshot" branch reported
    /// the disappearance as `complete` at 100% ("Done"). Every ingest
    /// failure — a 401 on a gated snapshot, a sha256 mismatch, a full
    /// disk — rendered as a successful install that installed nothing.
    /// Observed against `sep` (the gated `svrnmesh/sep-index` dataset)
    /// on 2026-07-26.
    ///
    /// `message` is the `Display` form of the `Error` that ended the
    /// ingest and is shown to the user verbatim, so errors that can
    /// reach here should read as guidance rather than as a bare status
    /// code (see `corpus_engine::Error::DownloadUnauthorized`).
    Failed {
        message: String,
    },
}
