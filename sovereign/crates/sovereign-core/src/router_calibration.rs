// SPDX-License-Identifier: AGPL-3.0-or-later
//! Threshold calibration for the router's embedding gates — the
//! measurement half of [`crate::router_axis`].
//!
//! ## The problem this closes
//!
//! The router ships twelve hand-picked threshold constants across six
//! axes, every one of them calibrated by hand against
//! Qwen3-Embedding-0.6B and justified in a source comment. Two of
//! those comments record decisions that turned on thousandths:
//! `archive_axis_live.rs` holds a negative out "by only 0.002 of
//! margin", and `router.rs` records a tool gate hijacked by "0.011 of
//! cosine noise". Constants that survive by two thousandths are not
//! calibrated; they are lucky, and reading the source cannot tell the
//! difference.
//!
//! They are also properties of the **encoder**, not of the task. Swap
//! the embedding model and all twelve are wrong simultaneously, with
//! no failing test to say so.
//!
//! ## How it works
//!
//! A [`ScoredCase`] is a labelled query that has already been scored
//! against an axis. Scoring costs one embedding; gating costs two
//! comparisons. So one embedding pass over a calibration bank makes
//! the entire threshold space searchable by arithmetic alone — the
//! same insight behind semantic-router's vectorised `_vec_evaluate`,
//! which reports a 34.85% → 90.91% accuracy jump from threshold
//! fitting alone.
//!
//! Two deliberate departures from that prior art:
//!
//! 1. **Exact, not sampled.** They draw 100 candidate thresholds from
//!    a `linspace` and random-search 500 times, which can miss the
//!    optimum. A threshold only changes behaviour when it crosses an
//!    observed score, so the complete set of distinct outcomes is
//!    enumerable: [`candidates`] returns the MIDPOINTS between
//!    consecutive observed values. That covers every reachable
//!    confusion matrix, and picking the midpoint places the boundary
//!    as far as possible from both neighbouring observations — an
//!    SVM-style max-margin choice in one dimension, so the winner
//!    comes with headroom rather than sitting on a knife edge.
//!
//! 2. **The objective encodes the asymmetry.** They optimise plain
//!    accuracy, which treats a false positive and a false negative as
//!    equally bad. Every axis in this router documents the opposite: a
//!    false positive hard-commits the turn down a narrowed path, while
//!    a false negative merely falls through to the pre-existing
//!    cascade. [`Objective::SafeRecall`] is the default for that
//!    reason; [`Objective::Accuracy`] is kept so the two can be
//!    compared on the same bank.
//!
//! ## What it does NOT do
//!
//! It does not write thresholds back into source. It reports the
//! current gate and the best gate side by side and leaves the edit to
//! a human — a calibration that silently rewrites the constants it is
//! measuring would be the same opaque loop this replaces.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::router_axis::{AxisGate, AxisScore};

/// A labelled query, already scored against one axis.
///
/// `expect` and `predicted` are both `Option<String>` so one engine
/// serves the binary axes (where `predicted` is always the positive
/// class) and the multi-class intent axis (where `predicted` is the
/// argmax intent and firing the WRONG intent is distinct from firing
/// when it should have abstained).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCase {
    pub id: String,
    /// The raw, ungated score. Independent of any threshold — this is
    /// what makes the sweep free.
    pub score: AxisScore,
    /// What the axis SHOULD produce. `None` means "must abstain".
    pub expect: Option<String>,
    /// What the axis WOULD produce if the gate admits this score.
    pub predicted: Option<String>,
}

impl ScoredCase {
    /// Did this case decide correctly under `gate`?
    ///
    /// Admitted → the decision is `predicted` and must equal `expect`.
    /// Rejected → the decision is "abstain" and is correct exactly
    /// when `expect` is `None`.
    pub fn is_correct(&self, gate: AxisGate) -> bool {
        if gate.admits(self.score) {
            self.predicted.is_some() && self.predicted == self.expect
        } else {
            self.expect.is_none()
        }
    }
}

/// Confusion counts for one candidate gate over one bank, plus the
/// headroom that produced them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateOutcome {
    pub min_sim: f32,
    pub min_margin: f32,
    /// Fired, and fired the right label.
    pub fired_correct: usize,
    /// Fired when it should have abstained. The expensive error: this
    /// is the one that hard-commits a turn down the wrong path.
    pub false_positive: usize,
    /// Fired for a case that should have fired, but with the wrong
    /// label. Only reachable on the multi-class intent axis.
    pub mislabelled: usize,
    /// Abstained, and abstaining was right.
    pub abstained_correct: usize,
    /// Abstained on a case that should have fired. The cheap error:
    /// the turn falls through to the cascade that predates the axis.
    pub missed: usize,
    /// Smallest cushion among cases the gate ADMITTED — how close the
    /// weakest accepted case sits to flipping off. `None` when nothing
    /// fired.
    pub weakest_accept: Option<f32>,
    /// Largest cushion among cases the gate REJECTED (so, `<= 0`) —
    /// how close the nearest rejected case came to firing. This is the
    /// "held out by 0.002" number. `None` when nothing was rejected.
    pub nearest_miss: Option<f32>,
}

impl GateOutcome {
    pub fn gate(&self) -> AxisGate {
        AxisGate::new(self.min_sim, self.min_margin)
    }

    pub fn total(&self) -> usize {
        self.fired_correct + self.false_positive + self.mislabelled + self.abstained_correct
            + self.missed
    }

    pub fn fired(&self) -> usize {
        self.fired_correct + self.false_positive + self.mislabelled
    }

    /// Every error, cheap and expensive alike.
    pub fn wrong(&self) -> usize {
        self.false_positive + self.mislabelled + self.missed
    }

    /// Fraction of ALL cases the gate committed on. On the intent axis
    /// this is the number that matters most: coverage is what displaces
    /// the ~1.2s LLM classifier call.
    pub fn coverage(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            return 0.0;
        }
        self.fired() as f64 / t as f64
    }

    /// Of the cases it committed on, how many were right.
    pub fn precision(&self) -> f64 {
        let f = self.fired();
        if f == 0 {
            return 1.0; // vacuously precise; coverage is what ranks it
        }
        self.fired_correct as f64 / f as f64
    }

    /// Of the cases that SHOULD have fired, how many did — correctly.
    pub fn recall(&self) -> f64 {
        let positives = self.fired_correct + self.mislabelled + self.missed;
        if positives == 0 {
            return 1.0;
        }
        self.fired_correct as f64 / positives as f64
    }

    /// Plain exact-match accuracy — what semantic-router's `fit()`
    /// optimises. Reported for comparison, not used as the default
    /// objective.
    pub fn accuracy(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            return 0.0;
        }
        (self.fired_correct + self.abstained_correct) as f64 / t as f64
    }

    /// Total headroom between the weakest accepted case and the
    /// nearest rejected one. Larger = the gate sits in a wider empty
    /// band = less likely to flip under an encoder change or a bank
    /// edit. This is the tie-breaker that keeps the search off knife
    /// edges.
    pub fn separation(&self) -> f32 {
        match (self.weakest_accept, self.nearest_miss) {
            (Some(a), Some(m)) => a - m,
            (Some(a), None) => a,
            (None, Some(m)) => -m,
            (None, None) => 0.0,
        }
    }
}

/// How to rank candidate gates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Objective {
    /// Maximise correct fires subject to a false-positive ceiling.
    ///
    /// The default, because it is the asymmetry every axis in this
    /// router already documents: firing wrongly hard-commits, failing
    /// to fire merely falls through. `max_false_positives: 0` is the
    /// shipped posture for the archive and locator axes.
    SafeRecall { max_false_positives: usize },
    /// Maximise plain exact-match accuracy. Kept so a bank can be
    /// scored the way the prior art scores it.
    Accuracy,
    /// Maximise coverage subject to a floor on precision-among-fired.
    ///
    /// The intent-axis objective: the embed router is worth having
    /// only insofar as it OWNS decisions, so the question is not "is it
    /// accurate" (it is, on everything it commits to) but "how much can
    /// it own before accuracy slips".
    MaxCoverage { min_precision: f64 },
}

impl Objective {
    /// Is this outcome admissible under the objective's constraint?
    fn feasible(&self, o: &GateOutcome) -> bool {
        match self {
            Objective::SafeRecall {
                max_false_positives,
            } => o.false_positive <= *max_false_positives,
            Objective::Accuracy => true,
            Objective::MaxCoverage { min_precision } => o.precision() >= *min_precision,
        }
    }

    /// Primary score among feasible outcomes. Higher is better.
    fn rank(&self, o: &GateOutcome) -> f64 {
        match self {
            Objective::SafeRecall { .. } => o.fired_correct as f64,
            Objective::Accuracy => o.accuracy(),
            Objective::MaxCoverage { .. } => o.coverage(),
        }
    }
}

/// Distinct threshold candidates for one dimension of the sweep.
///
/// A threshold only changes behaviour when it crosses an observed
/// value, so the midpoints between consecutive observations enumerate
/// every reachable outcome — and each midpoint sits as far as possible
/// from the two observations bracketing it, so the chosen gate carries
/// headroom by construction rather than landing on a sample.
///
/// Two sentinels bracket the list: one below the minimum (admit
/// everything on this axis) and one above the maximum (admit nothing).
pub fn candidates(observed: &[f32]) -> Vec<f32> {
    let mut vs: Vec<f32> = observed.to_vec();
    vs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    vs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    if vs.is_empty() {
        return vec![0.0];
    }
    // A tenth of the tightest observed gap, floored — small enough
    // never to straddle two observations, large enough to survive f32.
    const PAD: f32 = 1e-3;
    let mut out = Vec::with_capacity(vs.len() + 1);
    out.push(vs[0] - PAD); // admit everything
    for w in vs.windows(2) {
        out.push((w[0] + w[1]) / 2.0);
    }
    out.push(vs[vs.len() - 1] + PAD); // admit nothing
    out
}

/// Score one candidate gate against a whole bank.
pub fn evaluate(cases: &[ScoredCase], gate: AxisGate) -> GateOutcome {
    let mut o = GateOutcome {
        min_sim: gate.min_sim,
        min_margin: gate.min_margin,
        fired_correct: 0,
        false_positive: 0,
        mislabelled: 0,
        abstained_correct: 0,
        missed: 0,
        weakest_accept: None,
        nearest_miss: None,
    };
    for c in cases {
        let cushion = gate.cushion(c.score);
        if gate.admits(c.score) {
            o.weakest_accept = Some(match o.weakest_accept {
                Some(w) => w.min(cushion),
                None => cushion,
            });
            match (&c.expect, &c.predicted) {
                (None, _) => o.false_positive += 1,
                (Some(e), Some(p)) if e == p => o.fired_correct += 1,
                (Some(_), _) => o.mislabelled += 1,
            }
        } else {
            o.nearest_miss = Some(match o.nearest_miss {
                Some(m) => m.max(cushion),
                None => cushion,
            });
            if c.expect.is_none() {
                o.abstained_correct += 1;
            } else {
                o.missed += 1;
            }
        }
    }
    o
}

/// Exhaustively sweep the reachable threshold space and return the
/// best gate under `objective`, together with the outcome of the gate
/// currently shipped.
///
/// Returns `None` only for an empty bank.
pub fn fit(
    cases: &[ScoredCase],
    current: AxisGate,
    objective: Objective,
) -> Option<FitReport> {
    if cases.is_empty() {
        return None;
    }
    let sims: Vec<f32> = cases.iter().map(|c| c.score.sim_positive).collect();
    let margins: Vec<f32> = cases.iter().map(|c| c.score.margin()).collect();
    let sim_cands = candidates(&sims);
    let margin_cands = candidates(&margins);

    let mut best: Option<GateOutcome> = None;
    let mut evaluated = 0usize;
    for &s in &sim_cands {
        for &m in &margin_cands {
            let o = evaluate(cases, AxisGate::new(s, m));
            evaluated += 1;
            if !objective.feasible(&o) {
                continue;
            }
            let better = match &best {
                None => true,
                Some(b) => {
                    let (ro, rb) = (objective.rank(&o), objective.rank(b));
                    // Primary objective, then fewest total errors, then
                    // the widest empty band around the boundary.
                    (ro, b.wrong(), o.separation()) > (rb, o.wrong(), b.separation())
                }
            };
            if better {
                best = Some(o);
            }
        }
    }

    Some(FitReport {
        current: evaluate(cases, current),
        best,
        cases_scored: cases.len(),
        gates_evaluated: evaluated,
    })
}

/// The side-by-side a human needs to decide whether to move a
/// constant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitReport {
    /// How the SHIPPED gate scores on this bank.
    pub current: GateOutcome,
    /// The best reachable gate under the objective. `None` when the
    /// objective's constraint is unsatisfiable on this bank (e.g. a
    /// zero-false-positive requirement that no gate can meet).
    pub best: Option<GateOutcome>,
    pub cases_scored: usize,
    pub gates_evaluated: usize,
}

impl FitReport {
    /// Would moving to `best` change any decision on this bank?
    ///
    /// A `false` here is the healthy steady state: the shipped gate is
    /// already optimal for this bank, and the interesting number
    /// becomes `current.separation()` — how much headroom it has.
    pub fn would_change(&self) -> bool {
        match &self.best {
            None => false,
            Some(b) => {
                b.fired_correct != self.current.fired_correct
                    || b.false_positive != self.current.false_positive
                    || b.mislabelled != self.current.mislabelled
                    || b.missed != self.current.missed
            }
        }
    }
}

// ── Calibration bank (on-disk) ───────────────────────────────────

/// A calibration bank: labelled queries with the axis each one probes.
///
/// Deliberately NOT the `EvalBank` shape used by `sovereign eval`.
/// That bank is a full-cascade behavioural test — it needs a corpus,
/// runs the LLM, and carries `expected_facts` (routing-only banks fill
/// them with `"stub"` to satisfy validation). A calibration bank is
/// pure embedding geometry: no corpus, no LLM, milliseconds, and it
/// can express "this MUST abstain", which the eval bank cannot.
///
/// It is the data-file generalisation of the hardcoded arrays in
/// `tests/archive_axis_live.rs` and `tests/locator_axis_live.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationBank {
    pub bank: CalibrationMeta,
    #[serde(default)]
    pub case: Vec<CalibrationCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationCase {
    pub id: String,
    /// Which gate this case probes: `intent`, `locator`, `scope`,
    /// `archive`, `current_info`, or `effort`.
    pub axis: String,
    pub query: String,
    /// The label the axis must produce, or `"abstain"` for a case that
    /// must NOT fire. On the `intent` axis this is an intent name
    /// (`knowledge_query`, …); on a binary axis it is the positive
    /// class name or `"abstain"`.
    pub expect: String,
    /// Why this case exists. Load-bearing documentation: a calibration
    /// case without a reason is a case nobody can safely delete later.
    #[serde(default)]
    pub note: String,
}

impl CalibrationCase {
    /// `None` when the case must abstain.
    pub fn expected_label(&self) -> Option<&str> {
        match self.expect.trim() {
            "abstain" | "none" | "" => None,
            other => Some(other),
        }
    }
}

/// Axes a calibration bank may name. Parsing is total so a typo fails
/// at load rather than silently dropping cases from the sweep.
pub const KNOWN_AXES: &[&str] = &[
    "intent",
    "locator",
    "scope",
    "archive",
    "current_info",
    "effort",
];

pub fn parse_bank(raw: &str) -> Result<CalibrationBank, String> {
    let bank: CalibrationBank = toml::from_str(raw).map_err(|e| format!("parse: {e}"))?;
    validate_bank(&bank)?;
    Ok(bank)
}

fn validate_bank(bank: &CalibrationBank) -> Result<(), String> {
    if bank.bank.name.trim().is_empty() {
        return Err("bank.name is empty".into());
    }
    if bank.case.is_empty() {
        return Err("bank has zero cases".into());
    }
    let mut seen: HashSet<&str> = HashSet::with_capacity(bank.case.len());
    for c in &bank.case {
        if c.id.trim().is_empty() {
            return Err("case with empty id".into());
        }
        if !seen.insert(c.id.as_str()) {
            return Err(format!("duplicate case id `{}`", c.id));
        }
        if c.query.trim().is_empty() {
            return Err(format!("case `{}` has an empty query", c.id));
        }
        if !KNOWN_AXES.contains(&c.axis.as_str()) {
            return Err(format!(
                "case `{}` names unknown axis `{}` (known: {})",
                c.id,
                c.axis,
                KNOWN_AXES.join(", ")
            ));
        }
        if c.expect.trim().is_empty() {
            return Err(format!(
                "case `{}` has an empty `expect` (use \"abstain\" for a \
                 case that must not fire)",
                c.id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, sim_p: f32, sim_n: f32, expect: Option<&str>, pred: &str) -> ScoredCase {
        ScoredCase {
            id: id.into(),
            score: AxisScore::new(sim_p, sim_n),
            expect: expect.map(String::from),
            predicted: Some(pred.into()),
        }
    }

    /// A bank shaped like a real binary axis: positives cluster high,
    /// negatives low, with one negative sitting awkwardly close.
    fn binary_bank() -> Vec<ScoredCase> {
        vec![
            case("pos_clear", 0.70, 0.50, Some("archive"), "archive"),
            case("pos_mid", 0.62, 0.52, Some("archive"), "archive"),
            case("pos_weak", 0.55, 0.50, Some("archive"), "archive"),
            case("neg_near", 0.52, 0.48, None, "archive"),
            case("neg_far", 0.20, 0.40, None, "archive"),
        ]
    }

    #[test]
    fn candidates_bracket_and_interleave_observations() {
        let c = candidates(&[0.2, 0.5, 0.8]);
        assert_eq!(c.len(), 4, "3 observations → 2 midpoints + 2 sentinels");
        assert!(c[0] < 0.2, "first sentinel admits everything");
        assert!((c[1] - 0.35).abs() < 1e-6);
        assert!((c[2] - 0.65).abs() < 1e-6);
        assert!(c[3] > 0.8, "last sentinel admits nothing");
    }

    #[test]
    fn candidates_dedupe_identical_scores() {
        let c = candidates(&[0.5, 0.5, 0.5]);
        assert_eq!(c.len(), 2, "one distinct value → just the sentinels");
    }

    #[test]
    fn candidates_handle_the_empty_bank() {
        assert_eq!(candidates(&[]), vec![0.0]);
    }

    #[test]
    fn evaluate_counts_the_four_outcomes() {
        // Gate admits sim>=0.55 and margin>=0.04:
        //   pos_clear .70/.20 fire ✓   pos_mid .62/.10 fire ✓
        //   pos_weak  .55/.05 fire ✓   neg_near .52 floor-rejected ✓
        //   neg_far   .20 rejected ✓
        let o = evaluate(&binary_bank(), AxisGate::new(0.55, 0.04));
        assert_eq!(o.fired_correct, 3);
        assert_eq!(o.false_positive, 0);
        assert_eq!(o.missed, 0);
        assert_eq!(o.abstained_correct, 2);
        assert_eq!(o.total(), 5);
        assert!((o.accuracy() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn evaluate_reports_the_binding_cushions() {
        let o = evaluate(&binary_bank(), AxisGate::new(0.55, 0.04));
        // Weakest accept is pos_weak: floor headroom 0.00, margin
        // headroom 0.01 → 0.00 binds.
        assert!((o.weakest_accept.unwrap() - 0.0).abs() < 1e-6);
        // Nearest miss is neg_near: floor headroom -0.03, margin
        // headroom 0.00 → -0.03 binds.
        assert!((o.nearest_miss.unwrap() - -0.03).abs() < 1e-6);
        assert!((o.separation() - 0.03).abs() < 1e-6);
    }

    #[test]
    fn a_loose_gate_admits_the_false_positive() {
        let o = evaluate(&binary_bank(), AxisGate::new(0.45, 0.02));
        assert_eq!(o.false_positive, 1, "neg_near must slip through");
        assert_eq!(o.fired_correct, 3);
    }

    /// The headline behaviour: fitting finds a gate with ZERO false
    /// positives that still fires every positive, and it places the
    /// boundary in the empty band rather than on an observation.
    #[test]
    fn safe_recall_finds_a_zero_false_positive_gate_with_headroom() {
        let bank = binary_bank();
        let report = fit(
            &bank,
            AxisGate::new(0.45, 0.02),
            Objective::SafeRecall {
                max_false_positives: 0,
            },
        )
        .expect("non-empty bank");

        assert_eq!(report.current.false_positive, 1, "shipped gate leaks");
        let best = report.best.as_ref().expect("a safe gate exists");
        assert_eq!(best.false_positive, 0);
        assert_eq!(best.fired_correct, 3, "and loses no positives");
        assert!(report.would_change());

        // The chosen gate must sit strictly between observations, not
        // on one — that is what buys the headroom.
        assert!(
            best.separation() > 0.0,
            "expected a positive separation, got {}",
            best.separation()
        );
        assert!(
            best.weakest_accept.unwrap() > 0.0,
            "no accepted case should sit exactly on the boundary"
        );
    }

    #[test]
    fn safe_recall_reports_none_when_no_gate_can_satisfy_it() {
        // A positive and a negative with IDENTICAL scores: no
        // threshold can separate them, so a zero-FP gate that fires
        // anything is impossible... but abstaining entirely IS
        // feasible (0 false positives, 0 correct fires).
        let bank = vec![
            case("p", 0.6, 0.5, Some("archive"), "archive"),
            case("n", 0.6, 0.5, None, "archive"),
        ];
        let r = fit(
            &bank,
            AxisGate::new(0.5, 0.0),
            Objective::SafeRecall {
                max_false_positives: 0,
            },
        )
        .unwrap();
        let best = r.best.expect("abstain-everything is always feasible");
        assert_eq!(best.false_positive, 0);
        assert_eq!(best.fired_correct, 0, "it must give up the positive");
        assert_eq!(best.missed, 1);
    }

    #[test]
    fn accuracy_objective_will_trade_a_false_positive_for_recall() {
        // Two positives sit BELOW a negative, so any gate that fires
        // them also fires the negative. Accuracy accepts that trade
        // (2 right + 0 = 2/3 ... vs abstaining 1/3); SafeRecall refuses.
        let bank = vec![
            case("p1", 0.50, 0.40, Some("a"), "a"),
            case("p2", 0.52, 0.42, Some("a"), "a"),
            case("n1", 0.55, 0.45, None, "a"),
        ];
        let acc = fit(&bank, AxisGate::new(0.5, 0.0), Objective::Accuracy)
            .unwrap()
            .best
            .unwrap();
        assert_eq!(acc.fired_correct, 2);
        assert_eq!(acc.false_positive, 1, "accuracy tolerates the leak");

        let safe = fit(
            &bank,
            AxisGate::new(0.5, 0.0),
            Objective::SafeRecall {
                max_false_positives: 0,
            },
        )
        .unwrap()
        .best
        .unwrap();
        assert_eq!(safe.false_positive, 0);
        assert_eq!(safe.fired_correct, 0, "it abstains rather than leak");
    }

    /// Intent is multi-class: firing the WRONG label is a distinct
    /// error from firing when it should have abstained.
    #[test]
    fn mislabelled_is_counted_apart_from_false_positive() {
        let bank = vec![
            ScoredCase {
                id: "wrong_label".into(),
                score: AxisScore::new(0.80, 0.60),
                expect: Some("deep_query".into()),
                predicted: Some("knowledge_query".into()),
            },
            ScoredCase {
                id: "should_abstain".into(),
                score: AxisScore::new(0.80, 0.60),
                expect: None,
                predicted: Some("knowledge_query".into()),
            },
        ];
        let o = evaluate(&bank, AxisGate::new(0.5, 0.0));
        assert_eq!(o.mislabelled, 1);
        assert_eq!(o.false_positive, 1);
        assert_eq!(o.fired_correct, 0);
        assert_eq!(o.wrong(), 2);
    }

    #[test]
    fn max_coverage_respects_its_precision_floor() {
        // Three correct high scorers and one mislabelled low scorer.
        let bank = vec![
            case("a", 0.90, 0.60, Some("k"), "k"),
            case("b", 0.85, 0.60, Some("k"), "k"),
            case("c", 0.80, 0.60, Some("k"), "k"),
            ScoredCase {
                id: "d".into(),
                score: AxisScore::new(0.60, 0.55),
                expect: Some("deep_query".into()),
                predicted: Some("k".into()),
            },
        ];
        // Demanding perfection stops above the mislabelled case.
        let strict = fit(
            &bank,
            AxisGate::new(0.55, 0.02),
            Objective::MaxCoverage { min_precision: 1.0 },
        )
        .unwrap()
        .best
        .unwrap();
        assert_eq!(strict.fired_correct, 3);
        assert_eq!(strict.mislabelled, 0);
        assert!((strict.coverage() - 0.75).abs() < 1e-9);

        // Relaxing it lets the fourth in and coverage reaches 100%.
        let loose = fit(
            &bank,
            AxisGate::new(0.55, 0.02),
            Objective::MaxCoverage { min_precision: 0.7 },
        )
        .unwrap()
        .best
        .unwrap();
        assert!((loose.coverage() - 1.0).abs() < 1e-9);
        assert_eq!(loose.mislabelled, 1);
    }

    #[test]
    fn would_change_is_false_when_the_shipped_gate_is_already_optimal() {
        let bank = binary_bank();
        // The gate the `evaluate` test showed is already perfect.
        let r = fit(
            &bank,
            AxisGate::new(0.55, 0.04),
            Objective::SafeRecall {
                max_false_positives: 0,
            },
        )
        .unwrap();
        assert!(
            !r.would_change(),
            "an already-optimal gate must not be reported as movable"
        );
    }

    #[test]
    fn fit_returns_none_on_an_empty_bank() {
        assert!(fit(&[], AxisGate::new(0.5, 0.0), Objective::Accuracy).is_none());
    }

    #[test]
    fn is_correct_agrees_with_the_confusion_counts() {
        let gate = AxisGate::new(0.55, 0.04);
        for c in binary_bank() {
            let counted_right = {
                let o = evaluate(std::slice::from_ref(&c), gate);
                o.fired_correct + o.abstained_correct == 1
            };
            assert_eq!(c.is_correct(gate), counted_right, "case {}", c.id);
        }
    }

    // ── Bank parsing ─────────────────────────────────────────────

    #[test]
    fn parses_a_minimal_bank() {
        let src = r#"
[bank]
name = "demo"

[[case]]
id = "c1"
axis = "archive"
query = "Have I mentioned kayaking in any of our past chats?"
expect = "archive"
note = "the production failure this axis was built for"

[[case]]
id = "c2"
axis = "archive"
query = "What did Kant say about duty?"
expect = "abstain"
"#;
        let b = parse_bank(src).unwrap();
        assert_eq!(b.case.len(), 2);
        assert_eq!(b.case[0].expected_label(), Some("archive"));
        assert_eq!(b.case[1].expected_label(), None, "abstain → None");
    }

    #[test]
    fn rejects_an_unknown_axis() {
        let src = r#"
[bank]
name = "demo"
[[case]]
id = "c1"
axis = "vibes"
query = "?"
expect = "abstain"
"#;
        let e = parse_bank(src).unwrap_err();
        assert!(e.contains("unknown axis"), "got: {e}");
    }

    #[test]
    fn rejects_duplicate_case_ids() {
        let src = r#"
[bank]
name = "demo"
[[case]]
id = "c1"
axis = "scope"
query = "a"
expect = "abstain"
[[case]]
id = "c1"
axis = "scope"
query = "b"
expect = "abstain"
"#;
        assert!(parse_bank(src).unwrap_err().contains("duplicate"));
    }

    /// The SHIPPED bank must stay loadable and keep its shape.
    ///
    /// Without this, a typo'd axis name or a dropped `expect` surfaces
    /// only when someone runs `sovereign router fit` — which needs a
    /// GGUF on disk and so is not a CI gate. The bank is data, and
    /// data rots silently.
    #[test]
    fn shipped_calibration_bank_parses_and_keeps_its_shape() {
        const SHIPPED: &str =
            include_str!("../../../bench/routing/calibration/axes_v1.toml");
        let bank = parse_bank(SHIPPED).expect("shipped calibration bank must parse");

        // Every axis must be exercised — a gate with no cases is a
        // gate the fit command silently skips.
        for axis in KNOWN_AXES {
            assert!(
                bank.case.iter().any(|c| c.axis == *axis),
                "no calibration cases for the `{axis}` axis"
            );
        }

        // The abstain concept is the whole reason this bank exists
        // alongside the routing banks: nothing else in the repo tests
        // what must NOT be committed.
        let abstains = bank
            .case
            .iter()
            .filter(|c| c.expected_label().is_none())
            .count();
        assert!(
            abstains >= 15,
            "expected a substantial abstain set, found {abstains}"
        );

        // A threshold sweep needs both classes on every axis, or the
        // objective is trivially satisfiable.
        for axis in KNOWN_AXES {
            let cases: Vec<_> = bank.case.iter().filter(|c| c.axis == *axis).collect();
            assert!(
                cases.iter().any(|c| c.expected_label().is_some()),
                "axis `{axis}` has no positives"
            );
            assert!(
                cases.iter().any(|c| c.expected_label().is_none()),
                "axis `{axis}` has no abstain cases — its gate could be \
                 set to fire on everything and still score perfectly"
            );
        }

        // Every case carries its reason. See the bank header.
        for c in &bank.case {
            assert!(
                !c.note.trim().is_empty(),
                "case `{}` has no note explaining why it exists",
                c.id
            );
        }
    }

    #[test]
    fn rejects_an_empty_expect() {
        let src = r#"
[bank]
name = "demo"
[[case]]
id = "c1"
axis = "scope"
query = "a"
expect = ""
"#;
        assert!(parse_bank(src).unwrap_err().contains("empty `expect`"));
    }
}
