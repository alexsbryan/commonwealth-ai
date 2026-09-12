// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's read of `GET /internal/corpus/local/{corpus}/ingest/progress`
//! (`sovereign_mesh::lc_http::IngestProgress`).
//!
//! NOT a relocation. `IngestProgress` closes over
//! `corpus_engine::enrichment::state::EnrichmentState` (the phase file) and
//! `sovereign_tools::local_corpus::manager::IngestStats` (the receipt's
//! counts), neither of which has a home at this layer. The route keeps the
//! whole type; these are the fields a client reads, deserialised from the
//! SAME bytes (serde ignores the rest). `sovereign-mesh`'s `wire_view_drift`
//! test serialises the real `IngestProgress` and parses this from it, so a
//! rename on the route side is red there rather than a silent `None` here
//! (ARCH principle 5: a check with a failing input you can name; principle
//! 6: absence is reported, never defaulted — every field below that the
//! route always writes is NOT `serde(default)`).

use serde::{Deserialize, Serialize};

/// What `GET …/{corpus}/ingest/progress` answers, as a client reads it.
///
/// `Stats` is the receipt's counts. The route writes `IngestStats`; a
/// client that links `sovereign-tools` names it (the desktop, which hands
/// the counts to its completion screen), a client that does not passes
/// `serde_json::Value` through. The parameter is what keeps this crate
/// from naming a capability-layer type while the desktop's read of the
/// counts stays typed.
#[derive(Debug, Serialize, Deserialize)]
pub struct IngestProgressView<Stats = serde_json::Value> {
    /// The corpus being ingested.
    pub corpus_id: String,
    /// The live phase stamp, when one exists.
    pub state: Option<IngestPhaseView>,
    /// The terminal receipt, when the ingest half has ended.
    pub outcome: Option<IngestOutcomeView<Stats>>,
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
pub struct IngestOutcomeView<Stats = serde_json::Value> {
    /// The corpus the receipt is for.
    pub corpus_id: String,
    /// The job that produced it.
    pub job_id: String,
    /// Unix seconds when the ingest half finished, either way.
    pub finished_at: i64,
    /// `Some` on success — the counts, verbatim from the manager.
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    /// `Some` on failure, naming it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
