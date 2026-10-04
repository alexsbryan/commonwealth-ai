// SPDX-License-Identifier: AGPL-3.0-or-later
//! Perturbation engine + prompt renderer.
//!
//! Three named metamorphic transformations, each with a structurally
//! predicted relationship to the base case the scorer enforces:
//!
//!   * **DIR-P1 (anti-gestalt sensitivity)** — make exit expensive
//!     (illiquid wealth + strong ties) while the surface still screams
//!     "billionaire flees a wealth tax." The structural answer collapses;
//!     a label-matcher stays put. `expected_sign = −1`.
//!   * **DIR-P2 (saturation)** — raise the home rate *and* the best
//!     destination rate by the same amount. The differential — hence the
//!     structural answer — does not move. Catches an agent that learned
//!     "higher tax ⇒ more flight." `expected_sign = 0` (flat).
//!   * **INV-I1 (identity invariance)** — swap name/nationality/industry/
//!     narrative, hold every feature fixed. The decision must not move.
//!     `expected_sign = 0` (flat).
//!
//! Two render modes share the renderer: [`RenderMode::Full`] shows the
//! structured feature block; [`RenderMode::Stripped`] is the **negative
//! control** — identity text only, no features. A `paraphrase` flag
//! selects an alternate, semantically-identical wording; a decision that
//! flips on rephrasing was never mechanism-reasoning.

use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};

use super::case::Case;

/// Coordinated rate rise applied by the saturation test, in absolute
/// rate units (2 percentage points). Both home and destination move by
/// this amount, leaving the differential unchanged.
pub const SAT_DELTA: f64 = 0.02;

/// P1 makes wealth nearly illiquid …
pub const P1_LIQUID_FRAC: f64 = 0.05;
/// … and ties nearly immovable.
pub const P1_MOBILITY_COST: f64 = 0.85;

/// The metamorphic family a variant belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerturbKind {
    /// The unperturbed reference; not itself scored.
    Ref,
    /// Sensitivity (directional expectation).
    Dir,
    /// Invariance.
    Inv,
}

/// One transformation applied to a base case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    Base,
    DirP1,
    DirP2,
    InvI1,
}

impl Variant {
    /// All variants, base first (the base must be elicited before its
    /// perturbations so the scorer has a reference probability).
    pub fn all() -> [Variant; 4] {
        [
            Variant::Base,
            Variant::DirP1,
            Variant::DirP2,
            Variant::InvI1,
        ]
    }

    /// Stable snake-case label used in `ResultRow.variant` and the
    /// manifest.
    pub fn label(&self) -> &'static str {
        match self {
            Variant::Base => "base",
            Variant::DirP1 => "dir_p1",
            Variant::DirP2 => "dir_p2",
            Variant::InvI1 => "inv_i1",
        }
    }

    pub fn kind(&self) -> PerturbKind {
        match self {
            Variant::Base => PerturbKind::Ref,
            Variant::DirP1 | Variant::DirP2 => PerturbKind::Dir,
            Variant::InvI1 => PerturbKind::Inv,
        }
    }

    /// Structurally-predicted sign of `p(perturbed) − p(base)`:
    /// −1 (must drop), +1 (must rise), 0 (must stay flat). Base is
    /// unused (its delta is zero by definition).
    pub fn expected_sign(&self) -> i8 {
        match self {
            Variant::Base => 0,
            Variant::DirP1 => -1,
            Variant::DirP2 => 0,
            Variant::InvI1 => 0,
        }
    }

    /// Apply the transformation. `rng` is consumed only by `InvI1`
    /// (which resamples a fresh synthetic identity); the others are
    /// deterministic functions of the base case.
    pub fn apply(&self, base: &Case, rng: &mut StdRng) -> Case {
        match self {
            Variant::Base => base.clone(),
            Variant::DirP1 => {
                let mut c = base.clone();
                c.liquid_frac = P1_LIQUID_FRAC;
                c.mobility_cost = P1_MOBILITY_COST;
                c.id = format!("{}~p1", base.id);
                c
            }
            Variant::DirP2 => {
                let mut c = base.clone();
                c.home_rate += SAT_DELTA;
                c.best_dest_rate += SAT_DELTA;
                c.id = format!("{}~p2", base.id);
                c
            }
            Variant::InvI1 => base.swap_identity(rng),
        }
    }
}

/// How much of the case the prompt exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderMode {
    /// Identity + the structured feature block (the real agent sees
    /// this).
    Full,
    /// Identity text only — the feature-stripped negative control.
    Stripped,
}

impl RenderMode {
    pub fn label(&self) -> &'static str {
        match self {
            RenderMode::Full => "full",
            RenderMode::Stripped => "stripped",
        }
    }

    /// True for the negative-control render.
    pub fn is_control(&self) -> bool {
        matches!(self, RenderMode::Stripped)
    }
}

/// Render the user prompt for a case under a given mode and wording.
///
/// The structured-output schema (set on the request, not here) enforces
/// the `{decision, confidence}` shape; the prompt only names the choice
/// set so the model knows what it's deciding.
pub fn render_prompt(case: &Case, mode: RenderMode, paraphrase: bool) -> String {
    if paraphrase {
        render_b(case, mode)
    } else {
        render_a(case, mode)
    }
}

fn render_a(case: &Case, mode: RenderMode) -> String {
    let mut s = String::new();
    s.push_str(
        "You are an economic analyst modeling whether a high-net-worth individual relocates in response to a wealth tax.\n\n",
    );
    s.push_str(&format!(
        "Subject: {} ({}; {}).\n{}\n",
        case.name, case.nationality, case.industry, case.narrative
    ));
    if let RenderMode::Full = mode {
        s.push_str("\nSituation:\n");
        s.push_str(&format!("- Total wealth: €{:.0}M\n", case.wealth));
        s.push_str(&format!(
            "- Liquid (portable) share of wealth: {}\n",
            pct(case.liquid_frac)
        ));
        s.push_str(&format!(
            "- Mobility cost (ties/age/business-specificity, 0–1): {:.2}\n",
            case.mobility_cost
        ));
        s.push_str(&format!(
            "- Home annual wealth-tax rate: {}\n",
            pct(case.home_rate)
        ));
        s.push_str(&format!(
            "- Best reachable destination's annual rate: {}\n",
            pct(case.best_dest_rate)
        ));
        s.push_str(&format!(
            "- One-time exit tax: {} of wealth\n",
            pct(case.exit_tax)
        ));
        s.push_str(&format!(
            "- Probability the tax is actually sustained/enforced: {}\n",
            pct(case.enforcement)
        ));
    }
    s.push_str(
        "\nQuestion: Within the next year, will this person RELOCATE to the destination, STAY, or are they INDIFFERENT? Give your decision and your confidence (0–1) that they relocate.",
    );
    s
}

fn render_b(case: &Case, mode: RenderMode) -> String {
    // Semantically identical to render_a, different surface wording —
    // the paraphrase-robustness arm.
    let mut s = String::new();
    s.push_str(
        "Acting as a policy economist, assess how a wealthy individual responds to a wealth tax: do they move abroad or not?\n\n",
    );
    s.push_str(&format!(
        "Person: {}, from {}, active in {}.\n{}\n",
        case.name, case.nationality, case.industry, case.narrative
    ));
    if let RenderMode::Full = mode {
        s.push_str("\nFacts:\n");
        s.push_str(&format!("- Net worth: {:.0} million euros\n", case.wealth));
        s.push_str(&format!(
            "- Fraction of that wealth that is liquid/portable: {}\n",
            pct(case.liquid_frac)
        ));
        s.push_str(&format!(
            "- How costly it is to uproot (0–1, higher = harder): {:.2}\n",
            case.mobility_cost
        ));
        s.push_str(&format!(
            "- Yearly wealth-tax rate at home: {}\n",
            pct(case.home_rate)
        ));
        s.push_str(&format!(
            "- Yearly rate in the cheapest place they could move to: {}\n",
            pct(case.best_dest_rate)
        ));
        s.push_str(&format!(
            "- Departure (exit) tax charged once: {} of net worth\n",
            pct(case.exit_tax)
        ));
        s.push_str(&format!(
            "- Likelihood the tax is genuinely collected over time: {}\n",
            pct(case.enforcement)
        ));
    }
    s.push_str(
        "\nOver the coming twelve months, decide whether the subject will RELOCATE, STAY, or is INDIFFERENT, and state how confident you are (0–1) that they move.",
    );
    s
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mechanism_fidelity::structural::structural_p_relocate;
    use rand::SeedableRng;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(99)
    }

    #[test]
    fn p1_collapses_the_structural_delta() {
        let base = Case::base_example();
        let p1 = Variant::DirP1.apply(&base, &mut rng());
        let d = structural_p_relocate(&p1) - structural_p_relocate(&base);
        assert!(
            d < -0.5,
            "P1 must produce a large negative structural delta, got {d}"
        );
        assert_eq!(Variant::DirP1.expected_sign(), -1);
    }

    #[test]
    fn p2_holds_the_differential_and_stays_flat() {
        let base = Case::base_example();
        let p2 = Variant::DirP2.apply(&base, &mut rng());
        // Differential preserved …
        assert!(
            ((p2.home_rate - p2.best_dest_rate) - (base.home_rate - base.best_dest_rate)).abs()
                < 1e-12
        );
        // … so the structural answer does not move.
        let d = structural_p_relocate(&p2) - structural_p_relocate(&base);
        assert!(d.abs() < 1e-9, "P2 structural delta must be ~0, got {d}");
        assert_eq!(Variant::DirP2.expected_sign(), 0);
    }

    #[test]
    fn i1_changes_identity_but_no_feature() {
        let base = Case::base_example();
        let inv = Variant::InvI1.apply(&base, &mut rng());
        assert_ne!(inv.name, base.name, "identity must change");
        // Every mechanism feature identical → structural prior identical.
        assert_eq!(structural_p_relocate(&inv), structural_p_relocate(&base));
    }

    #[test]
    fn control_render_hides_features_so_dir_prompts_are_identical() {
        // The crux of negative-control validity: because DIR keeps
        // identity fixed and the narrative is mechanism-free, the
        // stripped base and stripped perturbed prompts are byte-equal —
        // so the control *cannot* see what changed.
        let base = Case::base_example();
        let p1 = Variant::DirP1.apply(&base, &mut rng());
        let base_ctrl = render_prompt(&base, RenderMode::Stripped, false);
        let p1_ctrl = render_prompt(&p1, RenderMode::Stripped, false);
        assert_eq!(
            base_ctrl, p1_ctrl,
            "control must be blind to DIR feature changes"
        );

        // The full render, by contrast, must differ (the features show).
        let base_full = render_prompt(&base, RenderMode::Full, false);
        let p1_full = render_prompt(&p1, RenderMode::Full, false);
        assert_ne!(base_full, p1_full);
    }

    #[test]
    fn full_render_contains_features_stripped_does_not() {
        let base = Case::base_example();
        let full = render_prompt(&base, RenderMode::Full, false);
        let stripped = render_prompt(&base, RenderMode::Stripped, false);
        assert!(full.contains("wealth-tax rate"));
        assert!(!stripped.contains("wealth-tax rate"));
        // Identity survives in both.
        assert!(full.contains(&base.name));
        assert!(stripped.contains(&base.name));
    }
}
