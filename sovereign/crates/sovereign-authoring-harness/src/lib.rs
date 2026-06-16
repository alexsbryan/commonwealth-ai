// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic authoring harness — the *policy + presentation* layer.
//!
//! Consumes the judgment-free `StageOutput`s the `corpus_engine::harness`
//! runner produces and turns them into an exact Pass / Fail verdict per stage,
//! with the failing items shown, not summarized. Pure functions of the form
//! `(stage output, declaration) -> Vec<Verdict>`; the renderer prints the
//! ladder. See `sovereign/docs/specs/AUTHORING_HARNESS.md`.
//!
//! The `Verdict` here lives in its own module path on purpose — `sovereign-eval`
//! already exports a different `Verdict` from `mechanism_fidelity::stopping`.

pub mod checks;
pub mod declaration;
pub mod render;

pub use checks::run_deterministic;
pub use declaration::Declaration;
pub use render::render_report;

use serde::{Deserialize, Serialize};

/// A full harness run: which sample, which recipe, and the per-stage verdicts.
/// Carries no timestamp — that lives in the capture sidecar — so the same
/// `(sample_id, recipe)` yields a byte-identical `HarnessRun` (I1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessRun {
    pub sample_id: String,
    pub recipe_hash: String,
    pub stages: Vec<StageResult>,
}

impl HarnessRun {
    /// The only roll-up (I5): green iff no stage carries a `Fail`. A `Warn`
    /// (e.g. a filter that dropped nothing) never gates.
    pub fn green(&self) -> bool {
        self.stages
            .iter()
            .all(|s| s.verdicts.iter().all(|v| v.status != Status::Fail))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: String,
    pub config_hash: String,
    pub cache_hit: bool,
    pub verdicts: Vec<Verdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub check: CheckId,
    pub status: Status,
    /// What the declaration promised — including the threshold, always on screen.
    pub expected: String,
    /// What actually happened.
    pub observed: String,
    /// The concrete failing/sample items — never just a count.
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Warn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    ExtractCoverage,
    FilterKept,
    ChunkDegeneracy,
    IndexRoundtrip,
    AcquireIntegrity,
    EnrichLinkIntegrity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub locus: Locus,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Locus {
    Doc(String),
    Chunk(String),
    Atom(String),
}

/// Stable hash of a config slice — used for `StageResult.config_hash` and, when
/// stage caching lands (I6), the cache key.
pub(crate) fn config_hash<T: Serialize>(value: &T) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
