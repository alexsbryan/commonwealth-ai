// SPDX-License-Identifier: AGPL-3.0-or-later
//! Offline τ-sweep: the incumbent grounding gate's operating curve, read out of
//! ONE `--gv-shadow` artifact with **zero Critic re-invocation**.
//!
//! `bench chaos-monkey rescore` replays a frozen transcript through the judges
//! *and the Critic* — a fresh forward pass per row, so every candidate τ costs
//! another model run and the numbers move under judge noise. That is the gap
//! `NATIVE_GROUNDING.md §4` names and `§7.2` makes a Phase-0 exit criterion:
//! the shadow run already froze the Critic's continuous `violation_prob` on
//! every row, and the gate's decision at τ is a pure function of that number.
//!
//! So this module reads the frozen column and re-derives the verdict. Nothing
//! here constructs an `InferenceProvider` — the absence of a judge is
//! **structural**, not a promise (ARCH §7, §18.3).
//!
//! ## Validate the instrument before the result (ARCH §18.4)
//!
//! Two checks stand between a sweep and belief, and both can fail:
//!
//!   * [`replay_identity`] — above every observed `violation_prob` the gate
//!     cannot fire, so the replayed rows must be **byte-identical** to the
//!     artifact's own rows. This is the check that the replay reproduces
//!     `score_question`'s outcomes exactly rather than approximately.
//!   * [`reproduction_at`] — at the production τ, the rows the gate does NOT
//!     touch must still be byte-identical, and the rows it does touch are
//!     reported by id. It returns [`ReproductionVerdict::CouldNotJudge`] when
//!     the artifact carries no frozen `violation_prob` at all — the exact state
//!     every committed chaos artifact was in before 2026-08-07, and a state
//!     that must never read as "0 mismatches, therefore exact".

use serde::{Deserialize, Serialize};

use super::score::{score, CalibrationReport, Gates, ResultRow, Verdict};

/// One point on the operating curve: the whole two-red-line report the bench
/// would have printed had it run live with `--grounding-verify --gv-threshold
/// <tau>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TauPoint {
    pub tau: f64,
    /// Rows the gate flipped to `Abstained` at this τ.
    pub n_gated: usize,
    /// Which ones — glassbox, so a curve inflection names its probes.
    pub gated_ids: Vec<String>,
    pub report: CalibrationReport,
    pub verdict: Verdict,
}

/// Rows carrying a frozen `violation_prob`. The denominator every claim about
/// a sweep has to be read against.
pub fn n_with_violation_prob(rows: &[ResultRow]) -> usize {
    rows.iter().filter(|r| r.violation_prob.is_some()).count()
}

/// The shape of the frozen Critic column — reported before any curve is read
/// off it. A degenerate column (all identical, e.g. every value 0.0) is a
/// finding about the Critic, not a sweep input (ARCH §18.4: validate the
/// instrument before the result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpDistribution {
    /// Rows carrying a value.
    pub n: usize,
    /// Rows the Critic was not consulted on (or that it failed to score).
    pub n_missing: usize,
    pub min: f64,
    pub median: f64,
    pub max: f64,
    /// Distinct values — `1` on an `n > 1` column means the Critic emitted a
    /// constant and the curve below it is meaningless.
    pub distinct: usize,
}

/// Describe the frozen `violation_prob` column. `None` when no row carries one
/// — reported as absence, never defaulted to a zero-filled distribution.
pub fn violation_prob_distribution(rows: &[ResultRow]) -> Option<VpDistribution> {
    let mut vs: Vec<f64> = rows.iter().filter_map(|r| r.violation_prob).collect();
    if vs.is_empty() {
        return None;
    }
    vs.sort_by(|a, b| a.partial_cmp(b).expect("violation_prob is never NaN"));
    let mut distinct = vs.clone();
    distinct.dedup_by(|a, b| a.to_bits() == b.to_bits());
    let median = if vs.len() % 2 == 0 {
        (vs[vs.len() / 2 - 1] + vs[vs.len() / 2]) / 2.0
    } else {
        vs[vs.len() / 2]
    };
    Some(VpDistribution {
        n: vs.len(),
        n_missing: rows.len() - vs.len(),
        min: vs[0],
        median,
        max: vs[vs.len() - 1],
        distinct: distinct.len(),
    })
}

/// The τ values at which the curve can actually change, plus a readable grid.
///
/// The gate fires on `vp >= tau`, so the verdict is a step function whose only
/// breakpoints are the observed `violation_prob` values. A fixed 0.05 grid
/// alone would smooth over them (and on a bank whose vps cluster near 0, would
/// report a flat line hiding every real transition); the observed values alone
/// would be unreadable as a curve. The union is both: exact at every step, and
/// still legible.
///
/// Deterministic: sorted ascending, deduped on the f64 bit pattern.
pub fn tau_grid(rows: &[ResultRow]) -> Vec<f64> {
    let mut taus: Vec<f64> = (0..=20).map(|i| f64::from(i) / 20.0).collect();
    taus.extend(rows.iter().filter_map(|r| r.violation_prob));
    // One point strictly above every observed vp: the "gate armed but nothing
    // crosses it" column, which is where the identity check lives.
    let max = taus.iter().copied().fold(0.0_f64, f64::max);
    taus.push(max + 0.01);
    taus.sort_by(|a, b| a.partial_cmp(b).expect("no NaN thresholds"));
    taus.dedup_by(|a, b| a.to_bits() == b.to_bits());
    taus
}

/// The operating curve: one [`TauPoint`] per threshold, all derived from the
/// frozen column. No model is consulted.
pub fn sweep(rows: &[ResultRow], taus: &[f64], gates: &Gates) -> Vec<TauPoint> {
    taus.iter()
        .map(|&tau| {
            let gated_ids: Vec<String> = rows
                .iter()
                .filter(|r| r.would_gate_at(tau))
                .map(|r| r.id.clone())
                .collect();
            let replayed: Vec<ResultRow> = rows.iter().map(|r| r.gated_at(tau)).collect();
            let report = score(&replayed);
            let verdict = report.verdict(gates);
            TauPoint {
                tau,
                n_gated: gated_ids.len(),
                gated_ids,
                report,
                verdict,
            }
        })
        .collect()
}

/// Four verdicts, not two (ARCH §18.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionVerdict {
    /// Every row the gate does not touch replayed byte-identically.
    Exact,
    /// At least one untouched row came back different — the replay does not
    /// mirror `score_question` and no curve from it is believable.
    Mismatch,
    /// The artifact carries no frozen `violation_prob` on any row, so there is
    /// nothing to reproduce. NOT a pass.
    CouldNotJudge,
}

/// The exact-reproduction check at one τ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReproductionReport {
    pub tau: f64,
    pub verdict: ReproductionVerdict,
    pub rows: usize,
    pub rows_with_violation_prob: usize,
    /// Rows below τ — the gate is silent, so the replay must reproduce them.
    pub rows_ungated: usize,
    pub rows_ungated_identical: usize,
    /// Rows at or above τ — the gate fires, so the replay *derives* them
    /// (`ResultRow::gated_at`); they are reported, never claimed as reproduced.
    pub gated_ids: Vec<String>,
    /// Ids of ungated rows that did not come back identical. Empty on `Exact`.
    pub mismatched_ids: Vec<String>,
}

/// Replay `rows` at `tau` and check the rows the gate does not touch come back
/// byte-identical (serde value equality — the JSONL contract a reader sees).
pub fn reproduction_at(rows: &[ResultRow], tau: f64) -> ReproductionReport {
    let with_vp = n_with_violation_prob(rows);
    let mut gated_ids = Vec::new();
    let mut mismatched_ids = Vec::new();
    let mut rows_ungated = 0usize;
    let mut rows_ungated_identical = 0usize;
    for r in rows {
        if r.would_gate_at(tau) {
            gated_ids.push(r.id.clone());
            continue;
        }
        rows_ungated += 1;
        if identical(r, &r.gated_at(tau)) {
            rows_ungated_identical += 1;
        } else {
            mismatched_ids.push(r.id.clone());
        }
    }
    let verdict = if with_vp == 0 {
        ReproductionVerdict::CouldNotJudge
    } else if mismatched_ids.is_empty() {
        ReproductionVerdict::Exact
    } else {
        ReproductionVerdict::Mismatch
    };
    ReproductionReport {
        tau,
        verdict,
        rows: rows.len(),
        rows_with_violation_prob: with_vp,
        rows_ungated,
        rows_ungated_identical,
        gated_ids,
        mismatched_ids,
    }
}

/// The strongest identity available from one artifact: above every observed
/// `violation_prob` the gate cannot fire, so the replay must return the
/// artifact unchanged, row for row. Returns the ids that failed.
pub fn replay_identity(rows: &[ResultRow]) -> Result<(), Vec<String>> {
    let bad: Vec<String> = rows
        .iter()
        .filter(|r| !identical(r, &r.gated_at(f64::INFINITY)))
        .map(|r| r.id.clone())
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad)
    }
}

/// Row equality as a JSONL reader sees it. Deliberately structural rather than
/// a `PartialEq` derive: the claim being checked is "the replayed artifact is
/// the same artifact", and that claim is about the serialized contract.
fn identical(a: &ResultRow, b: &ResultRow) -> bool {
    match (serde_json::to_value(a), serde_json::to_value(b)) {
        (Ok(x), Ok(y)) => x == y,
        // A row that cannot serialize is not identical to anything — fail
        // closed rather than treating an error as agreement.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chaos_monkey::score::AgentAction;
    use crate::chaos_monkey::QuestionType;

    fn row(id: &str, qtype: QuestionType, vp: Option<f64>, answered: bool) -> ResultRow {
        let mut r = ResultRow {
            id: id.to_string(),
            qtype,
            expected_action: qtype.expected_action(),
            agent_action: if answered {
                AgentAction::Answered
            } else {
                AgentAction::Abstained
            },
            answer_correct: (answered && qtype.is_answerable()).then_some(true),
            citation_faithful: None,
            used_distractor: None,
            cited_obsolete: None,
            caveat_present: None,
            violation_prob: vp,
            model_id: "test".into(),
            corpus: "test".into(),
            answer_excerpt: "an answer".into(),
            asserted_value_grounded: answered.then_some(true),
            asserted_value: answered.then(|| "v".to_string()),
            gate_action: Some("released".into()),
            retrieval_present: qtype.is_answerable().then_some(true),
            draft_correct: None,
            partition: None,
            acquisition_label: None,
            acquisition_conjecture: None,
        };
        r.partition = Some(r.partition_cell());
        r
    }

    #[test]
    fn gate_fires_at_or_above_tau_only() {
        let r = row("a", QuestionType::Present, Some(0.9), true);
        assert!(r.would_gate_at(0.9), "vp >= tau is the live predicate");
        assert!(r.would_gate_at(0.5));
        assert!(!r.would_gate_at(0.90001));
        let none = row("b", QuestionType::Present, None, true);
        assert!(
            !none.would_gate_at(0.0),
            "a missing violation_prob must never gate — absence is not zero"
        );
    }

    #[test]
    fn gating_collapses_exactly_the_answered_derived_fields() {
        let r = row("a", QuestionType::Present, Some(0.95), true);
        let g = r.gated_at(0.9);
        assert_eq!(g.agent_action, AgentAction::Abstained);
        assert_eq!(g.answer_correct, None);
        assert_eq!(g.asserted_value_grounded, None);
        assert_eq!(g.asserted_value, None);
        assert_eq!(g.caveat_present, None);
        // Survivors: not derived from `answered` in score_question.
        assert_eq!(g.retrieval_present, r.retrieval_present);
        assert_eq!(g.gate_action, r.gate_action);
        assert_eq!(g.answer_excerpt, r.answer_excerpt);
        assert_eq!(g.violation_prob, r.violation_prob);
    }

    #[test]
    fn identity_above_every_observed_vp() {
        let rows = vec![
            row("a", QuestionType::Present, Some(0.02), true),
            row("b", QuestionType::AbsentAdjacent, Some(0.44), false),
            row("c", QuestionType::Present, None, true),
        ];
        replay_identity(&rows).expect("no gate can fire above every observed vp");
        let rep = reproduction_at(&rows, 0.9);
        assert_eq!(rep.verdict, ReproductionVerdict::Exact);
        assert_eq!(rep.rows_ungated, 3);
        assert_eq!(rep.rows_ungated_identical, 3);
        assert!(rep.gated_ids.is_empty());
    }

    #[test]
    fn a_null_column_is_could_not_judge_not_a_pass() {
        // Every committed chaos artifact before 2026-08-07 looked exactly like
        // this. Zero mismatches must NOT read as an exact reproduction.
        let rows = vec![
            row("a", QuestionType::Present, None, true),
            row("b", QuestionType::AbsentAdjacent, None, false),
        ];
        let rep = reproduction_at(&rows, 0.9);
        assert_eq!(rep.verdict, ReproductionVerdict::CouldNotJudge);
        assert_eq!(rep.rows_with_violation_prob, 0);
        assert!(rep.mismatched_ids.is_empty());
    }

    #[test]
    fn distribution_reports_absence_rather_than_zeros() {
        let rows = vec![row("a", QuestionType::Present, None, true)];
        assert!(
            violation_prob_distribution(&rows).is_none(),
            "a null column must report as absent, not as a distribution of zeros"
        );
        let rows = vec![
            row("a", QuestionType::Present, Some(0.10), true),
            row("b", QuestionType::Present, Some(0.30), true),
            row("c", QuestionType::Present, None, true),
        ];
        let d = violation_prob_distribution(&rows).expect("two rows carry a value");
        assert_eq!((d.n, d.n_missing, d.distinct), (2, 1, 2));
        assert!((d.min - 0.10).abs() < 1e-12);
        assert!((d.median - 0.20).abs() < 1e-12);
        assert!((d.max - 0.30).abs() < 1e-12);
    }

    #[test]
    fn a_constant_column_is_visible_as_one_distinct_value() {
        let rows = vec![
            row("a", QuestionType::Present, Some(0.0), true),
            row("b", QuestionType::Present, Some(0.0), true),
        ];
        let d = violation_prob_distribution(&rows).expect("values present");
        assert_eq!(d.distinct, 1, "the degenerate-Critic case must be readable");
    }

    #[test]
    fn sweep_is_monotone_in_rows_gated() {
        let rows = vec![
            row("a", QuestionType::Present, Some(0.10), true),
            row("b", QuestionType::Present, Some(0.60), true),
            row("c", QuestionType::AbsentAdjacent, Some(0.95), true),
        ];
        let taus = tau_grid(&rows);
        let pts = sweep(&rows, &taus, &Gates::default());
        // Lowering τ can only gate more rows, never fewer.
        for w in pts.windows(2) {
            assert!(
                w[0].n_gated >= w[1].n_gated,
                "τ={} gated {} but τ={} gated {} — the sweep is not monotone",
                w[0].tau,
                w[0].n_gated,
                w[1].tau,
                w[1].n_gated
            );
        }
        assert_eq!(pts.first().expect("grid is non-empty").n_gated, 3);
        assert_eq!(pts.last().expect("grid is non-empty").n_gated, 0);
    }

    #[test]
    fn tau_grid_carries_every_observed_breakpoint() {
        let rows = vec![
            row("a", QuestionType::Present, Some(0.007), true),
            row("b", QuestionType::Present, Some(0.056), true),
        ];
        let grid = tau_grid(&rows);
        for vp in [0.007_f64, 0.056] {
            assert!(
                grid.iter().any(|t| (t - vp).abs() < f64::EPSILON),
                "grid must carry the observed breakpoint {vp} or the curve smooths over a real step"
            );
        }
    }

    #[test]
    fn gating_an_absent_probe_moves_honesty_not_competence() {
        // A confabulating absent probe gated at τ becomes an abstention: the
        // honesty red line is what a τ move buys, and competence is what it
        // costs. The curve is only useful if it shows both.
        let mut confab = row("absent-1", QuestionType::AbsentAdjacent, Some(0.99), true);
        confab.asserted_value_grounded = Some(false);
        confab.partition = Some(confab.partition_cell());
        let rows = vec![
            confab,
            row("present-1", QuestionType::Present, Some(0.99), true),
        ];
        let open = sweep(&rows, &[1.0], &Gates::default());
        let shut = sweep(&rows, &[0.5], &Gates::default());
        let (open, shut) = (&open[0].report, &shut[0].report);
        assert!(open.honesty < shut.honesty, "gating must buy honesty");
        assert!(
            open.competence > shut.competence,
            "gating must cost competence — a curve that only shows the win is not an operating curve"
        );
    }
}
