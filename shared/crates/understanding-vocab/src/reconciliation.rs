// SPDX-License-Identifier: AGPL-3.0-or-later
//! The multi-origin reconciler's vocabulary: its policy knobs, the merge
//! signals it tags, and its per-canonical-entity output, the data of
//! `atlas/reconciliation.json`. Moved from corpus-engine's
//! `enrichment::reconciliation` (phase-b pb-cli-llm-bench-move), which keeps
//! the merger and re-exports these at their historical paths, so bench can
//! read a reconciliation without naming the engine.

use serde::{Deserialize, Serialize};

use crate::atoms::{AtomId, SignalProvenance};
use crate::ontology::IdentityPolicy;

/// Tag for a signal that fired on a candidate pair. Round-trips
/// through the oplog so an auditor can reconstruct exactly which
/// signals supported a merge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeSignal {
    /// Canonical names + aliases agree under a fold (lowercase + ASCII
    /// punctuation collapse).
    NameSimilarity,
    /// One of the surface forms is an email address that resolves to
    /// the other party's company / domain.
    EmailHeader,
    /// Person + organisation + role all triangulate (Ken Lay @ Enron
    /// CEO ↔ Kenneth Lay @ Enron CEO).
    OrgRole,
    /// The calibrated judge confirmed the merge after the
    /// reconciliation policy escalated.
    JudgeConfirmed,
    /// Same email-thread root in the carrier doc's metadata (Phase 2
    /// thread_id matches → the two mentions are in the same
    /// conversation).
    ThreadRoot,
    /// Every external identifier the recipe declared for this type
    /// (`identity = ["rxnorm_id"]`) is present on both and agrees. The one
    /// STRICT signal: it satisfies the cross-origin gate on its own, because
    /// an identifier is a criterion of identity, not evidence toward one.
    ExternalId,
    /// Every descriptive key the recipe declared as the fallback
    /// (`identity_fallback = ["name", "employer"]`) agrees. One ordinary
    /// signal — it goes through the same count gate every other signal does,
    /// which is what "a descriptive key is judged, not trusted" means today.
    DescriptiveKey,
    Other(String),
}

impl MergeSignal {
    pub fn as_str(&self) -> &str {
        match self {
            MergeSignal::NameSimilarity => "name_similarity",
            MergeSignal::EmailHeader => "email_header",
            MergeSignal::OrgRole => "org_role",
            MergeSignal::JudgeConfirmed => "judge_confirmed",
            MergeSignal::ThreadRoot => "thread_root",
            MergeSignal::ExternalId => "external_id",
            MergeSignal::DescriptiveKey => "descriptive_key",
            MergeSignal::Other(s) => s.as_str(),
        }
    }
}

/// Policy knobs for the merger. Mirrors the
/// `[enrichment.reconciliation]` TOML schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationPolicy {
    /// Minimum overlap-similarity for a name match to count as a
    /// candidate. Today the
    /// [`super::signals::NameSimilaritySignal`] uses an exact
    /// fold-match; future versions may swap a real similarity
    /// function here.
    #[serde(default = "default_name_similarity_threshold")]
    pub name_similarity_threshold: f32,
    /// Minimum *distinct* signals required for a cross-origin merge.
    /// Two same-origin mentions (both LLM batch, both column header)
    /// can merge on `name_similarity` alone; two cross-origin
    /// mentions need a second signal (`email_header`, `org_role`, or
    /// `judge_confirmed`).
    ///
    /// Default 2 — Phase 5 tunes against the train split. Set to 1
    /// to recover the legacy single-signal behaviour.
    #[serde(default = "default_cross_origin_required_signals")]
    pub cross_origin_required_signals: u8,
    /// When `true`, the policy escalates uncertain candidates to the
    /// calibrated judge (`corpus-engine/assets/judges/business_entity_v1/`).
    /// The judge is owned by the runner — this primitive captures the
    /// outcome via `judge_callback`.
    #[serde(default = "default_true")]
    pub judge_when_uncertain: bool,
    /// Trial count fed into the judge harness when escalation fires.
    /// Matches `sovereign_agent_bench::judge_multi::run_judge_trials`'s
    /// `trials` parameter.
    #[serde(default = "default_judge_trials")]
    pub judge_trials: u8,
    /// Per-declared-type identity keys (ontology v1), already resolved through
    /// `specializes` by `TypeIndex::effective_identity_policy`.
    ///
    /// Empty by default and empty for every corpus that declares no ontology,
    /// which is what makes this addition invisible to Enron: with no keys the
    /// signal stack is [`super::signals::default_signals`] term for term, the
    /// blocking keys are the same four, and the strict gate below can never
    /// fire. It is serialized into `reconciliation.json` so the criterion a
    /// merge ran under is on disk beside the merge.
    #[serde(default)]
    pub identity: IdentityPolicy,
}

impl Default for ReconciliationPolicy {
    fn default() -> Self {
        Self {
            name_similarity_threshold: default_name_similarity_threshold(),
            cross_origin_required_signals: default_cross_origin_required_signals(),
            judge_when_uncertain: default_true(),
            judge_trials: default_judge_trials(),
            identity: IdentityPolicy::default(),
        }
    }
}

fn default_name_similarity_threshold() -> f32 {
    0.85
}
fn default_cross_origin_required_signals() -> u8 {
    2
}
fn default_judge_trials() -> u8 {
    3
}
fn default_true() -> bool {
    true
}

/// Output record per canonical entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciledEntity {
    pub canonical_id: AtomId,
    pub canonical_name: String,
    /// Every (surface_form, provenance) the merger collapsed under
    /// `canonical_id`. Surface forms intentionally hold the verbatim
    /// canonical_name from the input atom — the recipe-author can
    /// inspect them for atypical surface variants.
    pub surface_forms: Vec<(String, SignalProvenance)>,
    pub signals_fired: Vec<MergeSignal>,
    /// The atom ids the merger collapsed; the oplog already carries
    /// these but we surface them here so a runtime read of the
    /// reconciled atlas doesn't require also opening the oplog.
    pub source_atom_ids: Vec<AtomId>,
}
