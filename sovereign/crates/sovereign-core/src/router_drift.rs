// SPDX-License-Identifier: AGPL-3.0-or-later
//! Score-distribution drift for the router's six embedding axes.
//!
//! ## The question `fit` cannot answer
//!
//! [`crate::router_calibration`] answers "is the shipped gate the best
//! one reachable on this bank, today". That is a snapshot, and it is
//! blind to the failure this system actually has: the ground moves
//! while the constants stay still. A new embedding model, a
//! re-quantised one, an edited exemplar bank — each shifts every
//! cosine on every axis without touching a line of
//! `scope_classifier.rs`. The archive negative that was held out "by
//! only 0.002 of margin" is one such shift from being admitted, and
//! today the only signal would be a bench regression noticed days
//! later, by hand.
//!
//! So: persist the fit report, and diff the next one against it.
//!
//! ## Why a dated baseline and not a metrics pipeline
//!
//! The comparison needs exactly two points — the last recorded run and
//! this one — and the repo already has a storage convention for
//! precisely that shape (`bench_cmd::baselines`: dated JSON plus a
//! `latest.json` symlink, one directory per bench). A time series
//! would add a collector, a retention policy and a query surface in
//! order to answer a question two files answer.
//!
//! ## What makes two runs comparable
//!
//! Three conditions. All are RECORDED rather than assumed, because
//! each one silently invalidates the deltas when it fails:
//!
//! * **Same encoder.** Cosines from two models are coordinates in
//!   different spaces. Subtracting them yields a number, not a
//!   measurement.
//! * **Same bank.** Adding calibration cases moves `separation`
//!   legitimately — that is better measurement, not drift. It is the
//!   same trap [`crate::router_calibration`] warns about in the
//!   fitting direction, and it bites just as hard here.
//! * **Same gate.** A moved constant is a deliberate act by a human.
//!   The report names it rather than folding it into a delta, where it
//!   would read as the encoder having moved.
//!
//! When a condition fails the deltas are still printed — they are the
//! operator's evidence, and hiding them would be the opposite of the
//! point — but no regression is CLAIMED. An unattributable difference
//! is a reason to look, never a verdict.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::router_axis::AxisGate;
use crate::router_calibration::FitReport;

/// Separation changes smaller than this are backend noise, not drift.
///
/// A thousandth is the resolution these axes are argued at — the two
/// incidents that motivated calibration at all turned on 0.002 of
/// margin and 0.011 of cosine — so the floor sits just under the
/// smallest difference anyone has had to reason about.
///
/// It exists for the cross-machine case. With the same gate, bank and
/// encoder, separation is deterministic and any real change is a
/// distribution shift; but the same GGUF on a different GPU backend
/// can differ in the last f32 places, and a check that reports drift
/// every time it runs somewhere new reports nothing at all.
pub const DRIFT_EPS: f32 = 0.001;

/// One `router fit` run, persisted so a later one can be diffed
/// against it.
///
/// Carries its own provenance because the deltas are only meaningful
/// under it — see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitSnapshot {
    /// The embedding model file every cosine in `axes` came from.
    pub embed_model: String,
    /// Bank paths, for the human reading the JSON.
    pub banks: Vec<String>,
    /// Content digest of those banks — the machine-checkable half of
    /// `banks`.
    pub bank_digest: String,
    /// Per-axis fit, keyed by axis name.
    pub axes: BTreeMap<String, FitReport>,
}

/// Stable digest of the calibration bank(s) a snapshot was measured
/// against.
///
/// CONTENT, not path or mtime: moving a bank or touching it must not
/// read as a changed bank, and editing one case must. The separator
/// keeps concatenation unambiguous, so splitting one bank into two
/// files is a change even when the bytes are otherwise identical —
/// which it is, since the axes are grouped per file.
pub fn bank_digest(sources: &[String]) -> String {
    let mut h = Sha256::new();
    for s in sources {
        h.update(s.as_bytes());
        h.update([0u8]);
    }
    let full = format!("{:x}", h.finalize());
    full[..16].to_string()
}

/// What happened to one axis between two snapshots.
#[derive(Debug, Clone, PartialEq)]
pub enum AxisChange {
    /// Scored now, absent from the baseline — a new axis, or one whose
    /// cases were all skipped last time.
    Appeared,
    /// In the baseline, missing now. Usually a classifier that failed
    /// to build, which is worth surfacing rather than silently
    /// dropping.
    Vanished,
    /// Present in both.
    Compared(AxisDelta),
}

/// The shipped gate's behaviour, before and after.
///
/// Every field is the SHIPPED gate's, never the fitted best: drift
/// asks what happened to the constants in production, not to the
/// optimum the sweep could reach.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisDelta {
    pub gate_before: AxisGate,
    pub gate_after: AxisGate,
    pub separation_before: f32,
    pub separation_after: f32,
    pub weakest_accept_before: Option<f32>,
    pub weakest_accept_after: Option<f32>,
    pub nearest_miss_before: Option<f32>,
    pub nearest_miss_after: Option<f32>,
    pub errors_before: usize,
    pub errors_after: usize,
    pub coverage_before: f64,
    pub coverage_after: f64,
    pub cases_before: usize,
    pub cases_after: usize,
}

impl AxisDelta {
    fn between(before: &FitReport, after: &FitReport) -> Self {
        let (b, a) = (&before.current, &after.current);
        Self {
            gate_before: b.gate(),
            gate_after: a.gate(),
            separation_before: b.separation(),
            separation_after: a.separation(),
            weakest_accept_before: b.weakest_accept,
            weakest_accept_after: a.weakest_accept,
            nearest_miss_before: b.nearest_miss,
            nearest_miss_after: a.nearest_miss,
            errors_before: b.wrong(),
            errors_after: a.wrong(),
            coverage_before: b.coverage(),
            coverage_after: a.coverage(),
            cases_before: before.cases_scored,
            cases_after: after.cases_scored,
        }
    }

    /// Negative = the gate moved closer to a knife edge.
    pub fn d_separation(&self) -> f32 {
        self.separation_after - self.separation_before
    }

    /// Positive = the shipped gate makes more mistakes than it did.
    pub fn d_errors(&self) -> i64 {
        self.errors_after as i64 - self.errors_before as i64
    }

    pub fn d_coverage(&self) -> f64 {
        self.coverage_after - self.coverage_before
    }

    /// Somebody edited one of the twelve constants since the baseline.
    pub fn gate_moved(&self) -> bool {
        self.gate_before != self.gate_after
    }

    /// The bank grew or shrank on this axis, so the deltas describe a
    /// different measurement rather than a different distribution.
    pub fn cases_changed(&self) -> bool {
        self.cases_before != self.cases_after
    }

    /// Did this axis get worse?
    ///
    /// Two independent ways, and whether the gate moved decides which
    /// of them apply:
    ///
    /// * **More errors on the shipped gate.** Always a regression. It
    ///   is the outcome the axis exists to prevent, and it means the
    ///   same thing whether or not a human moved a constant — indeed a
    ///   constant moved into a worse position is exactly the edit this
    ///   should catch.
    /// * **Less separation.** Only meaningful when the gate did NOT
    ///   move. Under a fixed gate, bank and encoder separation is
    ///   deterministic, so a drop is the score distribution closing in
    ///   on the boundary. When the gate DID move, a separation change
    ///   is the direct arithmetic consequence of that edit, and
    ///   calling it drift would blame the encoder for a human's
    ///   decision.
    pub fn regressed(&self) -> bool {
        if self.d_errors() > 0 {
            return true;
        }
        !self.gate_moved() && self.d_separation() < -DRIFT_EPS
    }
}

/// One axis's entry in a [`DriftReport`].
#[derive(Debug, Clone, PartialEq)]
pub struct AxisDrift {
    pub axis: String,
    pub change: AxisChange,
}

impl AxisDrift {
    /// `Some` only when the axis was present in both snapshots.
    pub fn delta(&self) -> Option<&AxisDelta> {
        match &self.change {
            AxisChange::Compared(d) => Some(d),
            _ => None,
        }
    }
}

/// The full before/after, plus the provenance that says whether it can
/// be believed.
#[derive(Debug, Clone)]
pub struct DriftReport {
    pub baseline_model: String,
    pub current_model: String,
    pub baseline_digest: String,
    pub current_digest: String,
    /// Union of both snapshots' axes, in a stable order.
    pub axes: Vec<AxisDrift>,
}

impl DriftReport {
    pub fn same_model(&self) -> bool {
        self.baseline_model == self.current_model
    }

    pub fn same_bank(&self) -> bool {
        self.baseline_digest == self.current_digest
    }

    /// The deltas mean what they appear to mean only when both hold.
    pub fn attributable(&self) -> bool {
        self.same_model() && self.same_bank()
    }

    /// Axes that got worse.
    ///
    /// EMPTY when the comparison is not attributable, by design: with
    /// a changed encoder or a changed bank, every number in the report
    /// legitimately moved, and claiming a regression there would train
    /// the reader to ignore the one that matters.
    pub fn regressions(&self) -> Vec<&AxisDrift> {
        if !self.attributable() {
            return Vec::new();
        }
        self.axes
            .iter()
            .filter(|a| a.delta().is_some_and(AxisDelta::regressed))
            .collect()
    }

    pub fn is_regression(&self) -> bool {
        !self.regressions().is_empty()
    }

    /// Axes whose shipped constants differ from the baseline's.
    /// Reported whether or not the run is attributable — a moved
    /// constant is a fact about the tree, not about the measurement.
    pub fn moved_gates(&self) -> Vec<&AxisDrift> {
        self.axes
            .iter()
            .filter(|a| a.delta().is_some_and(AxisDelta::gate_moved))
            .collect()
    }
}

/// Diff `current` against `baseline`, axis by axis.
pub fn compare(baseline: &FitSnapshot, current: &FitSnapshot) -> DriftReport {
    let mut names: Vec<&String> = baseline.axes.keys().chain(current.axes.keys()).collect();
    names.sort();
    names.dedup();

    let axes = names
        .into_iter()
        .filter_map(|name| {
            let change = match (baseline.axes.get(name), current.axes.get(name)) {
                (Some(b), Some(c)) => AxisChange::Compared(AxisDelta::between(b, c)),
                (None, Some(_)) => AxisChange::Appeared,
                (Some(_), None) => AxisChange::Vanished,
                // Unreachable: `name` came from one of the two maps.
                (None, None) => return None,
            };
            Some(AxisDrift {
                axis: name.clone(),
                change,
            })
        })
        .collect();

    DriftReport {
        baseline_model: baseline.embed_model.clone(),
        current_model: current.embed_model.clone(),
        baseline_digest: baseline.bank_digest.clone(),
        current_digest: current.bank_digest.clone(),
        axes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router_calibration::GateOutcome;

    /// A shipped-gate outcome with the two knobs these tests turn:
    /// how much headroom it has, and how many mistakes it makes.
    fn outcome(gate: AxisGate, weakest: f32, nearest: f32, errors: usize) -> GateOutcome {
        GateOutcome {
            min_sim: gate.min_sim,
            min_margin: gate.min_margin,
            fired_correct: 10,
            false_positive: errors,
            mislabelled: 0,
            abstained_correct: 10,
            missed: 0,
            weakest_accept: Some(weakest),
            nearest_miss: Some(nearest),
        }
    }

    fn report(gate: AxisGate, weakest: f32, nearest: f32, errors: usize) -> FitReport {
        FitReport {
            current: outcome(gate, weakest, nearest, errors),
            best: None,
            cases_scored: 20,
            gates_evaluated: 1,
            positives: 10,
            negatives: 10,
        }
    }

    fn snapshot(model: &str, digest: &str, axes: Vec<(&str, FitReport)>) -> FitSnapshot {
        FitSnapshot {
            embed_model: model.into(),
            banks: vec!["axes_v1.toml".into()],
            bank_digest: digest.into(),
            axes: axes.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }

    const GATE: AxisGate = AxisGate::new(0.50, 0.04);

    /// The steady state, and the one that must be silent: nothing
    /// changed, so nothing is reported.
    #[test]
    fn a_run_against_itself_is_clean() {
        let s = snapshot("qwen-0.6b", "abc", vec![("archive", report(GATE, 0.10, -0.05, 0))]);
        let d = compare(&s, &s);
        assert!(d.attributable());
        assert!(!d.is_regression());
        let delta = d.axes[0].delta().unwrap();
        assert_eq!(delta.d_separation(), 0.0);
        assert_eq!(delta.d_errors(), 0);
        assert!(!delta.gate_moved());
    }

    /// The whole point. Same gate, same bank, same encoder — and the
    /// weakest accepted case has drifted toward the boundary. No error
    /// count has moved yet, which is exactly why a human would never
    /// have caught this by reading the bench.
    #[test]
    fn a_cushion_closing_on_the_boundary_is_a_regression() {
        let before = snapshot("qwen-0.6b", "abc", vec![("archive", report(GATE, 0.100, -0.05, 0))]);
        let after = snapshot("qwen-0.6b", "abc", vec![("archive", report(GATE, 0.002, -0.05, 0))]);
        let d = compare(&before, &after);

        assert!(d.attributable());
        assert!(d.is_regression(), "a 0.098 loss of headroom must be caught");
        assert_eq!(d.regressions().len(), 1);
        assert_eq!(d.regressions()[0].axis, "archive");
        let delta = d.axes[0].delta().unwrap();
        assert!(delta.d_separation() < 0.0);
        assert_eq!(delta.d_errors(), 0, "and it fires before any error appears");
    }

    /// The same encoder on a different backend differs in the last f32
    /// places. A check that cries drift on every new machine is a check
    /// nobody reads.
    #[test]
    fn movement_below_the_epsilon_is_noise_not_drift() {
        let before = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.1000, -0.05, 0))]);
        let after = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.0995, -0.05, 0))]);
        let d = compare(&before, &after);
        assert!(d.axes[0].delta().unwrap().d_separation() < 0.0, "it did move");
        assert!(!d.is_regression(), "but not by enough to mean anything");
    }

    /// More mistakes is a regression under any circumstances — it is
    /// the outcome the axis exists to prevent.
    #[test]
    fn more_errors_is_always_a_regression() {
        let before = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.10, -0.05, 0))]);
        let after = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.10, -0.05, 1))]);
        assert!(compare(&before, &after).is_regression());
    }

    /// Cosines from two encoders are coordinates in different spaces.
    /// The deltas are still shown — they are the evidence — but the
    /// tool must not claim they mean anything.
    #[test]
    fn a_changed_encoder_is_reported_but_never_blamed() {
        let before = snapshot("qwen-0.6b", "abc", vec![("archive", report(GATE, 0.30, -0.05, 0))]);
        let after = snapshot("gemma-300m", "abc", vec![("archive", report(GATE, 0.01, -0.05, 3))]);
        let d = compare(&before, &after);

        assert!(!d.same_model());
        assert!(!d.attributable());
        assert!(
            d.regressions().is_empty(),
            "a cross-encoder difference is not a regression, however large"
        );
        // The evidence survives for the human.
        assert!(d.axes[0].delta().unwrap().d_separation() < -0.2);
        assert_eq!(d.axes[0].delta().unwrap().d_errors(), 3);
    }

    /// Adding calibration cases moves separation legitimately. That is
    /// better measurement, not drift — and it is the trap this whole
    /// toolchain was built to stop repeating.
    #[test]
    fn an_edited_bank_is_reported_but_never_blamed() {
        let before = snapshot("qwen-0.6b", "abc", vec![("effort", report(GATE, 0.30, -0.05, 0))]);
        let after = snapshot("qwen-0.6b", "def", vec![("effort", report(GATE, 0.01, -0.05, 0))]);
        let d = compare(&before, &after);
        assert!(d.same_model());
        assert!(!d.same_bank());
        assert!(!d.attributable());
        assert!(d.regressions().is_empty());
    }

    /// A human moving a constant explains the separation change by
    /// itself. Naming the edit is useful; blaming the encoder for it
    /// is not.
    #[test]
    fn a_moved_constant_is_named_not_blamed() {
        let before = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.20, -0.05, 0))]);
        let after = snapshot(
            "qwen-0.6b",
            "abc",
            vec![("scope", report(AxisGate::new(0.60, 0.04), 0.02, -0.05, 0))],
        );
        let d = compare(&before, &after);

        let delta = d.axes[0].delta().unwrap();
        assert!(delta.gate_moved());
        assert_eq!(d.moved_gates().len(), 1);
        assert!(delta.d_separation() < -DRIFT_EPS, "separation did shrink");
        assert!(
            !d.is_regression(),
            "but the edit, not the distribution, explains it"
        );
    }

    /// ...unless the edit made the axis worse, which is precisely the
    /// mistake a calibration tool should refuse to let through.
    #[test]
    fn a_moved_constant_that_costs_errors_is_still_a_regression() {
        let before = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.20, -0.05, 0))]);
        let after = snapshot(
            "qwen-0.6b",
            "abc",
            vec![("scope", report(AxisGate::new(0.30, 0.00), 0.20, -0.05, 2))],
        );
        let d = compare(&before, &after);
        assert!(d.axes[0].delta().unwrap().gate_moved());
        assert!(d.is_regression());
    }

    /// An axis that stopped being scored is a build failure or a
    /// deleted bank section. Silently dropping it would present a
    /// five-axis run as a clean six-axis one.
    #[test]
    fn axes_appearing_and_vanishing_are_both_surfaced() {
        let before = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.1, -0.05, 0))]);
        let after = snapshot("qwen-0.6b", "abc", vec![("effort", report(GATE, 0.1, -0.05, 0))]);
        let d = compare(&before, &after);

        assert_eq!(d.axes.len(), 2, "the union, not the intersection");
        assert_eq!(d.axes[0].axis, "effort");
        assert_eq!(d.axes[0].change, AxisChange::Appeared);
        assert_eq!(d.axes[1].axis, "scope");
        assert_eq!(d.axes[1].change, AxisChange::Vanished);
        assert!(!d.is_regression(), "neither is a score-distribution claim");
    }

    /// The digest must key on what was measured, not on where it lives
    /// or when it was touched.
    #[test]
    fn the_bank_digest_is_content_addressed() {
        let a = vec!["case = 1".to_string()];
        assert_eq!(bank_digest(&a), bank_digest(&["case = 1".to_string()]));
        assert_ne!(bank_digest(&a), bank_digest(&["case = 2".to_string()]));
        // Order is part of the identity — the axes are grouped per
        // file, so a reordering is a different measurement layout.
        let two = vec!["a".to_string(), "b".to_string()];
        let flipped = vec!["b".to_string(), "a".to_string()];
        assert_ne!(bank_digest(&two), bank_digest(&flipped));
        // And splitting one file into two is not the same bank.
        assert_ne!(bank_digest(&["ab".to_string()]), bank_digest(&two));
    }

    /// The first run has nothing to diff against; the caller must not
    /// have to special-case an empty baseline into a fake regression.
    #[test]
    fn an_empty_baseline_yields_only_appearances() {
        let before = snapshot("qwen-0.6b", "abc", vec![]);
        let after = snapshot("qwen-0.6b", "abc", vec![("scope", report(GATE, 0.1, -0.05, 0))]);
        let d = compare(&before, &after);
        assert_eq!(d.axes.len(), 1);
        assert_eq!(d.axes[0].change, AxisChange::Appeared);
        assert!(!d.is_regression());
    }
}
