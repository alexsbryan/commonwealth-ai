// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two-gate decision rule shared by every embedding axis in the
//! router — extracted from the classifiers that apply it.
//!
//! ## Why this exists
//!
//! Five of the router's six embedding gates are the same rule with
//! different nouns:
//!
//! | axis | positive class | negative class | floor | margin |
//! |---|---|---|---|---|
//! | scope (`scope_classifier`) | personal | external | 0.45 | 0.02 |
//! | archive (`archive_classifier`) | archive | this-thread | 0.50 | 0.04 |
//! | current-info (`current_info_classifier`) | current | evergreen | 0.50 | 0.04 |
//! | effort (`effort_classifier`) | high | low | 0.30 | 0.04 |
//! | locator (`router_embed::locator_from_embedding`) | tagged | untagged (one-vs-rest) | 0.50 | 0.02 |
//!
//! Each computed `sim_positive`, `sim_negative`, `margin = the
//! difference`, then fired iff `sim_positive >= floor && margin >=
//! min_margin` — five copies of one rule, five copies of the
//! dimension-mismatch guard, five copies of the tracing block, and ten
//! constants that could only be tuned by editing source.
//!
//! ## What separating scoring from gating buys
//!
//! The score does not depend on the thresholds. Once a query is
//! scored, evaluating *any* candidate gate against it is two
//! comparisons — no embedding, no inference. That is what makes
//! [`crate::router_calibration`] able to sweep an entire threshold
//! space over a whole bank from one embedding pass, and it is why
//! `classify_from_embedding` is now a thin wrapper over
//! `score_from_embedding` rather than the other way round.
//!
//! ## The asymmetry, stated once
//!
//! Every axis in this router documents the same asymmetry in its own
//! words: a **false positive hard-commits** the turn down a narrowed
//! path (conversation-only answering, personal-corpora-only retrieval,
//! the agentic planner), while a **false negative merely falls
//! through** to the cascade that existed before the axis was added. So
//! the gates are not tuned for accuracy — they are tuned to keep false
//! positives at zero and take whatever recall that allows. [`cushion`]
//! makes the resulting headroom a number instead of a comment.
//!
//! [`cushion`]: AxisGate::cushion

/// Raw, UNGATED score of one binary axis against one query.
///
/// Both fields are cosine similarities against L2-normalised vectors,
/// so they live in `[-1, 1]` and are directly comparable to the gate
/// constants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisScore {
    /// Cosine to the positive class (its centroid, or the best tagged
    /// exemplar for the one-vs-rest locator axis).
    pub sim_positive: f32,
    /// Cosine to the negative class (its centroid, or the best
    /// UNtagged exemplar).
    pub sim_negative: f32,
}

impl AxisScore {
    pub fn new(sim_positive: f32, sim_negative: f32) -> Self {
        Self {
            sim_positive,
            sim_negative,
        }
    }

    /// `sim_positive - sim_negative`. The quantity the margin gate
    /// turns on: how decisively the positive class won, independent of
    /// how similar anything was in absolute terms.
    pub fn margin(&self) -> f32 {
        self.sim_positive - self.sim_negative
    }
}

/// The decision rule: fire iff the absolute floor AND the margin both
/// clear.
///
/// Both gates are load-bearing and neither implies the other. The
/// floor rejects a query that is too far from anything to be trusted
/// even when it wins its axis by a mile; the margin rejects a query
/// that sits near both classes and would flip on embedding noise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisGate {
    /// Floor on `sim_positive`.
    pub min_sim: f32,
    /// Floor on `margin`.
    pub min_margin: f32,
}

impl AxisGate {
    pub const fn new(min_sim: f32, min_margin: f32) -> Self {
        Self {
            min_sim,
            min_margin,
        }
    }

    /// Does this gate admit that score?
    pub fn admits(&self, s: AxisScore) -> bool {
        s.sim_positive >= self.min_sim && s.margin() >= self.min_margin
    }

    /// Signed distance from the decision boundary, in cosine units:
    /// the smaller of the two gates' headroom.
    ///
    /// - **Positive** — the score fires, with this much room to spare.
    ///   Shrinking either threshold's slack below this flips nothing.
    /// - **Negative** — the score does not fire, and this is how much
    ///   the gate would have to move (on its binding side) to admit it.
    ///
    /// This is the number that turns a comment into a measurement.
    /// `archive_axis_live.rs` records a negative "held out by only
    /// 0.002 of margin" and `router.rs` records a tool gate hijacked by
    /// "0.011 of cosine noise" — both were found by hand, days after
    /// the fact. `cushion` computes them.
    pub fn cushion(&self, s: AxisScore) -> f32 {
        (s.sim_positive - self.min_sim).min(s.margin() - self.min_margin)
    }
}

/// Cosine of two equal-length vectors that are already L2-normalised
/// (so the dot product IS the cosine).
///
/// Shared by every axis; each classifier previously carried its own
/// private copy.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// L2-normalise in place. No-op on the zero vector (which would
/// otherwise divide by zero and produce NaNs that silently poison
/// every downstream comparison).
pub fn normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATE: AxisGate = AxisGate::new(0.50, 0.04);

    #[test]
    fn margin_is_the_difference() {
        let s = AxisScore::new(0.645, 0.568);
        assert!((s.margin() - 0.077).abs() < 1e-5);
    }

    #[test]
    fn admits_only_when_both_gates_clear() {
        // Both clear.
        assert!(GATE.admits(AxisScore::new(0.60, 0.50)));
        // Floor clears, margin does not.
        assert!(!GATE.admits(AxisScore::new(0.60, 0.58)));
        // Margin clears, floor does not.
        assert!(!GATE.admits(AxisScore::new(0.45, 0.30)));
        // Neither.
        assert!(!GATE.admits(AxisScore::new(0.20, 0.19)));
    }

    /// The gates are `>=`, not `>` — but only for values the binary
    /// representation can hit exactly.
    ///
    /// This test originally asserted `AxisScore::new(0.50, 0.46)`
    /// clears a `min_margin` of 0.04 and FAILED: as f32,
    /// `0.50 - 0.46 = 0.03999999165534973`, which is below 0.04. The
    /// subtraction, not the comparison, is where the boundary moves.
    ///
    /// That is the reason [`crate::router_calibration::candidates`]
    /// proposes MIDPOINTS between observed scores rather than the
    /// observed values themselves. A threshold placed exactly on an
    /// observation is a coin flip at the ULP level; one placed halfway
    /// between two observations cannot be. The inclusive comparison is
    /// real, but nothing in production is allowed to depend on it.
    #[test]
    fn gates_are_inclusive_at_exactly_representable_boundaries() {
        // 0.5 and 0.25 are exact in binary; their difference is too.
        let gate = AxisGate::new(0.5, 0.25);
        let exactly_on_both = AxisScore::new(0.5, 0.25);
        assert_eq!(exactly_on_both.margin(), 0.25);
        assert!(gate.admits(exactly_on_both), "`>=` must include equality");
        assert_eq!(gate.cushion(exactly_on_both), 0.0, "zero headroom");
    }

    /// The companion warning, pinned so nobody "fixes" the midpoint
    /// logic back to observed values.
    #[test]
    fn a_boundary_built_from_inexact_decimals_can_fall_the_wrong_way() {
        let gate = AxisGate::new(0.50, 0.04);
        let nominally_on_the_boundary = AxisScore::new(0.50, 0.46);
        assert!(
            !gate.admits(nominally_on_the_boundary),
            "0.50-0.46 is 0.0399999… in f32, so this does NOT clear 0.04 — \
             thresholds must never be placed on an observed value"
        );
    }

    /// The real regression this abstraction was extracted to make
    /// visible: `voice_H09_journal_think_leak` scored sim 0.452 /
    /// margin +0.038 against the ORIGINAL (0.45, 0.04) gate — admitted
    /// on the floor, held out by 0.002 of margin alone.
    #[test]
    fn cushion_recovers_the_two_thousandths_near_miss() {
        let original = AxisGate::new(0.45, 0.04);
        let leak = AxisScore::new(0.452, 0.414); // margin +0.038
        assert!(!original.admits(leak), "it must not fire");
        let c = original.cushion(leak);
        assert!(
            (c - -0.002).abs() < 1e-5,
            "expected a -0.002 cushion, got {c}"
        );

        // Raising the floor to 0.50 (what shipped) moves the binding
        // side from margin to floor and widens the cushion 24x.
        let shipped = AxisGate::new(0.50, 0.04);
        assert!(!shipped.admits(leak));
        assert!((shipped.cushion(leak) - -0.048).abs() < 1e-5);
    }

    #[test]
    fn cushion_is_positive_exactly_when_the_gate_admits() {
        for (p, n) in [(0.60, 0.50), (0.60, 0.58), (0.45, 0.30), (0.20, 0.19)] {
            let s = AxisScore::new(p, n);
            assert_eq!(
                GATE.admits(s),
                GATE.cushion(s) >= 0.0,
                "admits/cushion disagree on ({p}, {n})"
            );
        }
    }

    #[test]
    fn cushion_takes_the_binding_side() {
        // Floor headroom 0.30, margin headroom 0.01 → margin binds.
        let s = AxisScore::new(0.80, 0.75);
        assert!((GATE.cushion(s) - 0.01).abs() < 1e-5);
        // Floor headroom 0.01, margin headroom 0.47 → floor binds.
        let s = AxisScore::new(0.51, 0.00);
        assert!((GATE.cushion(s) - 0.01).abs() < 1e-5);
    }

    #[test]
    fn dot_of_normalized_is_cosine() {
        let a = vec![0.6, 0.8];
        assert!((dot(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_produces_a_unit_vector() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn normalize_leaves_the_zero_vector_alone() {
        let mut v = vec![0.0, 0.0];
        normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0], "must not produce NaNs");
    }
}
