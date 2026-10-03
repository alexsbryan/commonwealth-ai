// SPDX-License-Identifier: AGPL-3.0-or-later
//! The epistemic probe's evidence ([`super::ProbeMode::Epistemic`];
//! pb-cli-llm-ingest-move): per question, the ledger's live signals on a
//! miss — the cross-corpus coverage verdict and the acquisition routes the
//! resolver ranks — over the corpora the session reads through ingest's port.
//! It replaced sovereign-cli-llm's `epistemic_demo` example.

use serde::{Deserialize, Serialize};

use crate::types::{AcquisitionRoute, GapCoverage};

/// One question's coverage verdict and acquisition conjecture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpistemicEvidence {
    /// [`super::ProbeQuestion::id`].
    pub id: String,
    /// `Some(why)` when the question could not be embedded; the fields
    /// below are then empty and make no claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The coverage probe's verdict. `None` = the probe did not run
    /// (`SOVEREIGN_COVERAGE_PROBE` off, or an empty embedding), which is not
    /// a `topic_uncovered` verdict.
    pub coverage: Option<CoverageEvidence>,
    /// The resolver's routes, best first; empty when it is disabled or
    /// ranked nothing.
    pub routes: Vec<AcquisitionRoute>,
    /// Wall time of the query embed.
    pub embed_ms: u64,
    /// Wall time of the coverage probe.
    pub probe_ms: u64,
    /// Wall time of route resolution.
    pub resolve_ms: u64,
}

/// What the coverage probe measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageEvidence {
    /// The verdict the similarity implies against the near-sim floor.
    pub verdict: GapCoverage,
    /// Best nearest-chunk cosine similarity across the corpora in scope.
    pub best_similarity: f32,
    /// The corpus that produced it; `None` when no corpus answered.
    pub best_corpus: Option<String>,
}
