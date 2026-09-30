// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment eval's report: what `svrn enrich eval --report` and
//! `svrn enrich eval-median --report` write, and bench's lanes read back.
//!
//! Moved from sovereign-cli-llm's `enrich_cmd::eval` and
//! `enrich_cmd::eval_median` (phase-b pb-cli-llm-bench-move): ingest writes
//! the report and bench reads it, and this is a leaf both admit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseScore {
    pub expected: usize,
    pub matched: usize,
    pub forbidden_total: usize,
    pub forbidden_hit: usize,
    /// Per-expected hit list — names pulled from the golden's
    /// `*_contains_any` field (first entry by convention) so the
    /// report's miss column is human-readable.
    pub misses: Vec<String>,
    pub forbidden_hits: Vec<String>,
    pub notes: Vec<String>,
    /// Total candidate artefacts the scorer saw for this axis — the
    /// extraction VOLUME. `#[serde(default)]` keeps pre-P0.2 baselines
    /// deserializable (they read as 0 candidates → rate `None`).
    #[serde(default)]
    pub candidates: usize,
    /// Candidates explained by NO expected entry and NO forbidden
    /// entry: extraction volume that earns zero credit and, before
    /// P0.2, carried zero cost. The adjudication sampler
    /// (`bench enrichment-adjudicate`) prices how much of it is junk.
    #[serde(default)]
    pub unmatched_count: usize,
    /// Up to [`UNMATCHED_SAMPLE_CAP`] labels of unmatched candidates,
    /// for the human report. The full set is recomputed on demand by
    /// the adjudicator; this is a preview, not the record.
    #[serde(default)]
    pub unmatched_samples: Vec<String>,
}

impl PhaseScore {
    /// Precision = TP / (TP + FP). When the model emitted zero atoms
    /// AND the golden expected zero atoms, precision is undefined and
    /// the phase is genuinely silent (`None`). When the golden expected
    /// atoms but the model produced none, precision is treated as 0.0
    /// — otherwise zero-recall failures fall out of `f1()` as `None`
    /// and never enter the aggregate, hiding the regression. This bit
    /// is the difference between "no scoreable artefacts" (a silent
    /// phase) and "tried and failed" (a recall=0 phase).
    pub fn precision(&self) -> Option<f32> {
        let denom = self.matched + self.forbidden_hit;
        if denom == 0 {
            // Two sub-cases:
            //  - expected == 0 → genuinely undefined, stay silent
            //  - expected > 0  → zero-recall failure, return 0.0 so
            //    f1() lands in the aggregate
            if self.expected == 0 {
                return None;
            }
            return Some(0.0);
        }
        Some(self.matched as f32 / denom as f32)
    }

    pub fn recall(&self) -> Option<f32> {
        if self.expected == 0 {
            return None;
        }
        Some(self.matched as f32 / self.expected as f32)
    }

    pub fn f1(&self) -> Option<f32> {
        let p = self.precision()?;
        let r = self.recall()?;
        if p + r == 0.0 {
            return Some(0.0);
        }
        Some(2.0 * p * r / (p + r))
    }

    /// Fraction of the axis's candidate pool no golden entry explains.
    /// `None` when the pool is empty (nothing extracted ≠ over-
    /// extraction). Deliberately NOT folded into precision: the :30
    /// forbidden-only FP contract stays for baseline compat; this is
    /// the parallel volume signal.
    pub fn unmatched_rate(&self) -> Option<f32> {
        if self.candidates == 0 {
            return None;
        }
        Some(self.unmatched_count as f32 / self.candidates as f32)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvalReport {
    pub corpus_id: String,
    pub golden_path: String,
    pub positions: Option<PhaseScore>,
    pub person_atoms: Option<PhaseScore>,
    pub concept_atoms: Option<PhaseScore>,
    pub work_atoms: Option<PhaseScore>,
    pub event_atoms: Option<PhaseScore>,
    pub state_atoms: Option<PhaseScore>,
    pub relation_atoms: Option<PhaseScore>,
    pub question_atoms: Option<PhaseScore>,
    pub claim_atoms: Option<PhaseScore>,
    pub discourse_act_distribution: Option<DiscourseActReport>,
    pub edges: Option<PhaseScore>,
    pub fault_lines: Option<PhaseScore>,
    pub open_questions: Option<PhaseScore>,
    pub configurations: Option<PhaseScore>,

    // v2 typed-extension axes (Argumentative). Each is scored under
    // `PhaseFilter::Atoms` when its golden axis is non-empty.
    //
    // `axis_scores` is the authoritative storage — keyed by
    // `TypedAxis.key`. The five named fields below mirror the
    // canonical map so existing JSON consumers / baseline diffs see
    // identical keys. New axes added to `AXIS_CATALOG` show up only
    // in `axis_scores`, not as new named fields.
    pub axis_scores: BTreeMap<String, PhaseScore>,
    pub mechanism_atoms: Option<PhaseScore>,
    pub named_position_atoms: Option<PhaseScore>,
    pub evidence_atoms: Option<PhaseScore>,
    pub opposition_atoms: Option<PhaseScore>,
    pub concession_atoms: Option<PhaseScore>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscourseActReport {
    pub total_claims: usize,
    pub act_counts: Vec<(String, usize)>,
    pub required_satisfied: bool,
    pub uniform_violation: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseSummary {
    /// Per-run F1 in `[0.0, 1.0]`. `None` for runs where the phase
    /// produced no scoreable artefacts. Length always equals
    /// `runs.len()`.
    pub f1s: Vec<Option<f32>>,
    /// Per-run match counts (`matched / expected`) for the human
    /// breakdown.
    pub match_counts: Vec<(usize, usize)>,
    /// Total forbidden hits across runs — surfaces when the model
    /// occasionally produces a forbidden atom even if the median
    /// run avoids it.
    pub forbidden_hits: usize,
    /// Notes from any run (deduplicated). A note like
    /// "field_skeleton.json not present" appearing across all runs
    /// vs only one is itself signal.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AggregatedReport {
    pub corpus_id: String,
    pub golden_path: String,
    pub runs: usize,
    pub positions: PhaseSummary,
    pub person_atoms: PhaseSummary,
    pub concept_atoms: PhaseSummary,
    pub work_atoms: PhaseSummary,
    pub event_atoms: PhaseSummary,
    pub state_atoms: PhaseSummary,
    pub relation_atoms: PhaseSummary,
    pub question_atoms: PhaseSummary,
    pub claim_atoms: PhaseSummary,
    pub edges: PhaseSummary,
    pub fault_lines: PhaseSummary,
    pub open_questions: PhaseSummary,
    pub configurations: PhaseSummary,
    /// Per-run aggregate F1 across all scoreable phases (mirrors
    /// what `enrich eval` prints at the bottom).
    pub aggregate_f1s: Vec<Option<f32>>,
}

impl PhaseSummary {
    /// Run-to-run F1 spread (max − min, fraction units) across the
    /// runs where this phase scored. `None` with fewer than two scored
    /// runs — one data point has no spread.
    pub fn spread(&self) -> Option<f32> {
        let scored: Vec<f32> = self.f1s.iter().flatten().copied().collect();
        if scored.len() < 2 {
            return None;
        }
        let min = scored.iter().copied().fold(f32::INFINITY, f32::min);
        let max = scored.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        Some(max - min)
    }
}

impl AggregatedReport {
    /// Per-axis run-to-run spread — the measured noise floor `bench
    /// all` folds into its regression threshold (P0.1: an axis whose
    /// baseline delta sits inside its own spread is noise, not
    /// regression). Keys match `EvalReport`'s legacy named axes.
    pub fn spreads(&self) -> std::collections::BTreeMap<String, f32> {
        let named: [(&str, &PhaseSummary); 12] = [
            ("positions", &self.positions),
            ("person_atoms", &self.person_atoms),
            ("concept_atoms", &self.concept_atoms),
            ("work_atoms", &self.work_atoms),
            ("event_atoms", &self.event_atoms),
            ("state_atoms", &self.state_atoms),
            ("relation_atoms", &self.relation_atoms),
            ("question_atoms", &self.question_atoms),
            ("claim_atoms", &self.claim_atoms),
            ("fault_lines", &self.fault_lines),
            ("open_questions", &self.open_questions),
            ("configurations", &self.configurations),
        ];
        named
            .into_iter()
            .filter_map(|(name, s)| s.spread().map(|sp| (name.to_string(), sp)))
            .collect()
    }
}
