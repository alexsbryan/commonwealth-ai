// SPDX-License-Identifier: AGPL-3.0-or-later
//! The structural prior — the reference response surface that supplies
//! the DIR oracles.
//!
//! This is a logistic model of a single net-incentive scalar: the
//! present value of tax *saved* by relocating, minus the cost of the
//! move. It is hand-specified here with placeholder coefficients
//! (`horizon = 12`, `k = 1`, `scale = 50`); a later work package
//! calibrates them against observed natural-experiment outcomes (the one
//! legitimate regression-against-reality layer). The *directional*
//! predictions the DIR battery enforces are robust to the coefficients —
//! only the absolute probabilities move under recalibration.
//!
//! Critically the saving term keys on the **differential**
//! `home_rate − best_dest_rate`, not the raw home rate. That is what
//! makes the P2 "saturation" test meaningful: raise both rates by the
//! same amount and the differential — hence the structural answer — does
//! not move, even though a label-matcher that learned "higher tax ⇒ more
//! flight" would lurch.
//!
//! **Documented omission (feeds mechanism revision, not agent
//! retraining):** this prior does not model the political-capture
//! channel — the wealthy lobbying to gut enforcement rather than
//! relocating. When a synthetically-faithful agent later misses the real
//! holdout in a capture-dominated regime, that divergence is the signal
//! to revise the *mechanism*, not to fit the agent to synthetic labels.

use super::case::Case;

/// Default coefficients. Placeholders pending natural-experiment
/// calibration; exposed as constants so the calibration work package can
/// see exactly what it is replacing.
pub const HORIZON: f64 = 12.0;
pub const FRICTION_K: f64 = 1.0;
pub const SCALE: f64 = 50.0;

/// Structural probability of relocation in `[0, 1]` under the default
/// coefficients.
pub fn structural_p_relocate(c: &Case) -> f64 {
    structural_p_relocate_with(c, HORIZON, FRICTION_K, SCALE)
}

/// Coefficient-parameterised form, for calibration sweeps and tests.
///
/// ```text
/// saving    = wealth · (home_rate − best_dest_rate) · horizon · enforcement
/// move_cost = wealth · (exit_tax + mobility_cost · (1 − liquid_frac) · k)
/// p         = σ((saving − move_cost) / scale)
/// ```
pub fn structural_p_relocate_with(c: &Case, horizon: f64, k: f64, scale: f64) -> f64 {
    let saving = c.wealth * (c.home_rate - c.best_dest_rate) * horizon * c.enforcement;
    let move_cost = c.wealth * (c.exit_tax + c.mobility_cost * (1.0 - c.liquid_frac) * k);
    sigmoid((saving - move_cost) / scale)
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mechanism_fidelity::case::Case;

    /// Tolerance for the doc's worked oracle values.
    const EPS: f64 = 2e-3;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn worked_oracle_base() {
        // B: wealth 500, liq .90, mob .20, home .03, dest .00, exit 0, enf .90 → ≈0.954
        let p = structural_p_relocate(&Case::base_example());
        assert!(close(p, 0.954), "base p={p} expected ≈0.954");
    }

    #[test]
    fn worked_oracle_p1_collapse() {
        // P1 (anti-gestalt): illiquid + strong ties → ≈0.008, Δ ≈ −0.95.
        let mut c = Case::base_example();
        c.liquid_frac = 0.05;
        c.mobility_cost = 0.85;
        let p = structural_p_relocate(&c);
        assert!(close(p, 0.008), "P1 p={p} expected ≈0.008");
        let d = p - structural_p_relocate(&Case::base_example());
        assert!(d < -0.9, "P1 structural delta {d} should be a large collapse");
    }

    #[test]
    fn worked_oracle_p2_saturation_is_flat() {
        // P2: home and destination both rise 2pp; differential unchanged → flat.
        let mut c = Case::base_example();
        c.home_rate += 0.02;
        c.best_dest_rate += 0.02;
        let p = structural_p_relocate(&c);
        let base = structural_p_relocate(&Case::base_example());
        assert!(close(p, base), "P2 p={p} should equal base {base} (saturation)");
    }

    #[test]
    fn worked_oracle_i1_identity_invariance_is_flat() {
        // I1: identity swapped, every feature fixed → structurally identical.
        let mut c = Case::base_example();
        c.name = "Mara Okonkwo".into();
        c.nationality = "Aurelian".into();
        c.industry = "technology".into();
        c.narrative = "totally different flavour".into();
        assert_eq!(
            structural_p_relocate(&c),
            structural_p_relocate(&Case::base_example()),
            "identity must not move the structural prior at all"
        );
    }

    // ── Directional partials the DIR battery enforces ──

    fn bump(base: &Case, f: impl Fn(&mut Case)) -> f64 {
        let mut c = base.clone();
        f(&mut c);
        structural_p_relocate(&c)
    }

    #[test]
    fn monotone_in_each_feature() {
        // Anchor on a case with a clearly positive net incentive so the
        // signs are unambiguous.
        let base = Case::base_example();
        let p0 = structural_p_relocate(&base);

        // ∂p/∂(home_rate − best_dest_rate) > 0  (raise the differential)
        assert!(bump(&base, |c| c.home_rate += 0.01) > p0);
        assert!(bump(&base, |c| c.best_dest_rate += 0.01) < p0);
        // ∂p/∂mobility_cost < 0
        assert!(bump(&base, |c| c.mobility_cost += 0.3) < p0);
        // ∂p/∂liquid_frac > 0  (more portable ⇒ cheaper move ⇒ more flight)
        assert!(bump(&base, |c| c.liquid_frac -= 0.3) < p0);
        // ∂p/∂exit_tax < 0
        assert!(bump(&base, |c| c.exit_tax += 0.1) < p0);
        // ∂p/∂enforcement > 0  (more credible tax ⇒ bigger saving)
        assert!(bump(&base, |c| c.enforcement -= 0.4) < p0);
    }
}
