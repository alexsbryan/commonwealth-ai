// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scorer + the Rust→Python result contract.
//!
//! Scoring is always on the **decision probability**, never the
//! rationale. For a perturbation we compare the agent's signed delta
//! `d_agent = p(perturbed) − p(base)` against the structural prior's
//! `d_struct`, and emit four *conditional* booleans. Each is `Some` only
//! when its precondition (on `|d_struct|`) holds, so a verdict never
//! counts a band that didn't apply:
//!
//!   * `direction_ok` — the agent moved the right way, checked only when
//!     the structural delta is meaningfully non-flat.
//!   * `magnitude_ok` — on a large predicted move (`|d_struct| >
//!     big_struct`) the agent moved at least `collapse_min`.
//!   * `flat_ok` — on a predicted-flat DIR case (saturation,
//!     `|d_struct| < small_struct`) the agent barely moved (`< flat_max`).
//!   * `invariance_ok` — on an INV case the agent barely moved
//!     (`< inv_max`).
//!
//! The band constants are loaded from the pre-registration manifest in
//! the orchestrator; [`Bands::default`] reproduces the doc's worked
//! values so the pure logic is testable without a manifest on disk.

use serde::{Deserialize, Serialize};

use super::perturb::PerturbKind;

/// Pass/fail thresholds for the metamorphic relations. Frozen in the
/// pre-registration manifest before any optimization run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bands {
    /// `|d_agent|` must reach this on a large predicted move.
    pub collapse_min: f64,
    /// Saturation flatness ceiling for `|d_agent|`.
    pub flat_max: f64,
    /// Identity-invariance ceiling for `|d_agent|`.
    pub inv_max: f64,
    /// `|d_struct|` above this triggers the magnitude band.
    pub big_struct: f64,
    /// `|d_struct|` below this triggers the flat band.
    pub small_struct: f64,
}

impl Default for Bands {
    fn default() -> Self {
        Bands {
            collapse_min: 0.40,
            flat_max: 0.10,
            inv_max: 0.05,
            big_struct: 0.50,
            small_struct: 0.05,
        }
    }
}

/// The four conditional metamorphic outcomes for one (base, perturbation)
/// pair. `None` means "band did not apply to this case."
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scores {
    pub direction_ok: Option<bool>,
    pub magnitude_ok: Option<bool>,
    pub flat_ok: Option<bool>,
    pub invariance_ok: Option<bool>,
}

impl Scores {
    /// All-`None` — the reference (base) variant is not scored.
    pub fn none() -> Self {
        Scores {
            direction_ok: None,
            magnitude_ok: None,
            flat_ok: None,
            invariance_ok: None,
        }
    }
}

/// Score one perturbation's agent delta against the structural delta.
pub fn score(kind: PerturbKind, d_agent: f64, d_struct: f64, bands: &Bands) -> Scores {
    match kind {
        PerturbKind::Ref => Scores::none(),
        PerturbKind::Inv => Scores {
            invariance_ok: Some(d_agent.abs() < bands.inv_max),
            ..Scores::none()
        },
        PerturbKind::Dir => {
            let big = d_struct.abs() > bands.big_struct;
            let flat = d_struct.abs() < bands.small_struct;
            Scores {
                // Direction is only meaningful when the prior predicts a
                // real move; on a predicted-flat case `flat_ok` carries
                // the verdict instead.
                direction_ok: if flat {
                    None
                } else {
                    Some(same_sign(d_agent, d_struct))
                },
                magnitude_ok: if big {
                    Some(d_agent.abs() >= bands.collapse_min)
                } else {
                    None
                },
                flat_ok: if flat {
                    Some(d_agent.abs() < bands.flat_max)
                } else {
                    None
                },
                invariance_ok: None,
            }
        }
    }
}

/// Strict same-sign test. A zero agent delta fails (no movement is wrong
/// when a move was predicted); this avoids `f64::signum`'s `0.0 → +1`
/// trap.
fn same_sign(a: f64, b: f64) -> bool {
    (a > 0.0 && b > 0.0) || (a < 0.0 && b < 0.0)
}

/// One emitted row — the JSONL contract the Python verdict reads.
///
/// One row per (model, base case, variant, render mode, paraphrase).
/// Base rows are emitted too (with zero deltas and `None` scores) so the
/// raw reference probabilities are auditable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultRow {
    pub model_id: String,
    /// The reasoning class id (e.g. `wealth_tax_relocation`). Lets one
    /// JSONL hold rows from several classes and a reader group by it.
    #[serde(default)]
    pub class: String,
    pub case_id: String,
    /// `train` | `dev` | `test`.
    pub pool: String,
    /// `base` | `dir_p1` | `dir_p2` | `inv_i1`.
    pub variant: String,
    /// `full` | `stripped`.
    pub render: String,
    pub paraphrase: bool,
    /// `true` when `render == "stripped"` (the negative control).
    pub control: bool,
    pub expected_sign: i8,
    pub k_draws: u32,
    /// Vote-frequency probability of relocation over K draws:
    /// `(#relocate + 0.5·#indifferent) / K`.
    pub p_freq: f64,
    /// Mean verbalized confidence-of-relocation over K draws (the free
    /// co-elicited estimator).
    pub p_verbal: f64,
    /// `p_freq(this) − p_freq(base)` within the same (render, paraphrase)
    /// context. Zero for base rows.
    pub d_agent: f64,
    /// `structural(this) − structural(base)`. Zero for base rows.
    pub d_struct: f64,
    pub direction_ok: Option<bool>,
    pub magnitude_ok: Option<bool>,
    pub flat_ok: Option<bool>,
    pub invariance_ok: Option<bool>,
    pub seed: u64,
    pub latency_ms: u64,
    // ── Early-stopping provenance (Train/Dev, logprob path) ──
    /// Cases actually elicited for this model before its verdict resolved
    /// (or the full battery when stopping did not trigger / Test pool).
    /// Same value across all of a model's rows.
    #[serde(default)]
    pub n_drawn: usize,
    /// True when this model resolved and skipped its remaining cases.
    #[serde(default)]
    pub stopped_early: bool,
    /// The headline magnitude-band (μ_mag) confidence interval at the
    /// model's stop point. `None` when stopping was off (Test pool).
    #[serde(default)]
    pub cs_lower: Option<f64>,
    #[serde(default)]
    pub cs_upper: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p1_collapse_scored_on_direction_and_magnitude() {
        let b = Bands::default();
        // Large predicted collapse; faithful agent collapses too.
        let s = score(PerturbKind::Dir, -0.93, -0.95, &b);
        assert_eq!(s.direction_ok, Some(true));
        assert_eq!(s.magnitude_ok, Some(true));
        assert_eq!(s.flat_ok, None);
        assert_eq!(s.invariance_ok, None);

        // Label-matcher stays put (no movement) on the same collapse →
        // direction and magnitude both fail.
        let s = score(PerturbKind::Dir, 0.0, -0.95, &b);
        assert_eq!(s.direction_ok, Some(false));
        assert_eq!(s.magnitude_ok, Some(false));

        // A tiny move in the *correct* direction passes direction but
        // still fails the magnitude band — the two are independent.
        let s = score(PerturbKind::Dir, -0.05, -0.95, &b);
        assert_eq!(s.direction_ok, Some(true));
        assert_eq!(s.magnitude_ok, Some(false));
    }

    #[test]
    fn p2_saturation_scored_flat_only() {
        let b = Bands::default();
        // Predicted flat (differential unchanged); faithful agent flat.
        let s = score(PerturbKind::Dir, 0.03, 0.0, &b);
        assert_eq!(s.flat_ok, Some(true));
        assert_eq!(s.direction_ok, None, "no direction band on a flat prediction");
        assert_eq!(s.magnitude_ok, None);

        // Agent that learned 'higher tax ⇒ flight' lurches → flat fails.
        let s = score(PerturbKind::Dir, 0.35, 0.0, &b);
        assert_eq!(s.flat_ok, Some(false));
    }

    #[test]
    fn invariance_scored_only_on_inv() {
        let b = Bands::default();
        assert_eq!(score(PerturbKind::Inv, 0.02, 0.0, &b).invariance_ok, Some(true));
        assert_eq!(score(PerturbKind::Inv, 0.4, 0.0, &b).invariance_ok, Some(false));
    }

    #[test]
    fn same_sign_treats_zero_as_no_movement() {
        assert!(!same_sign(0.0, -0.9));
        assert!(same_sign(-0.1, -0.9));
        assert!(!same_sign(0.1, -0.9));
    }
}
