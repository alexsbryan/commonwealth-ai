//! Case schema + seeded synthetic generator for the mechanism-fidelity
//! harness (reference mechanism: relocation under a wealth tax).
//!
//! A [`Case`] carries two disjoint field groups:
//!   * **mechanism features** — the seven quantities the *structural
//!     prior* consumes and that a mechanism-faithful agent SHOULD key on
//!     (`wealth`, `liquid_frac`, `mobility_cost`, `home_rate`,
//!     `best_dest_rate`, `exit_tax`, `enforcement`);
//!   * **identity** — `name`/`nationality`/`industry`/`narrative`, which
//!     must NOT affect the decision (the invariance axis).
//!
//! The generator is deterministic in its seed, so a `(seed, n)` pair
//! reproduces the same battery bit-for-bit — a precondition for the
//! pre-registration discipline (a battery you can't reproduce can't be
//! pre-registered).
//!
//! **Negative-control invariant.** `narrative` is deliberately
//! mechanism-free: it names the subject and flavour only, never a wealth
//! level, liquidity, ties, or a tax rate. The feature-stripped control
//! render shows *only* identity text, so for a DIR perturbation (which
//! changes hidden feature values but holds identity fixed) the control's
//! base and perturbed prompts are byte-identical. Any sensitivity the
//! control then shows is, by construction, a leak — which is exactly
//! what the control is built to catch. Encoding a feature into the
//! narrative would silently defeat that detector.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

/// One synthetic subject. Mechanism features are continuous; identity
/// fields are surface text the invariance test perturbs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    // ── mechanism features (SHOULD drive the decision) ──
    /// Total wealth in €M.
    pub wealth: f64,
    /// Portable share of wealth, 0–1.
    pub liquid_frac: f64,
    /// Friction to move (ties, age, business-specificity), 0–1.
    pub mobility_cost: f64,
    /// Annual home wealth-tax rate (e.g. `0.03`).
    pub home_rate: f64,
    /// Annual rate at the best reachable destination.
    pub best_dest_rate: f64,
    /// One-time exit cost as a fraction of wealth.
    pub exit_tax: f64,
    /// Probability the tax is actually sustained/collected, 0–1.
    pub enforcement: f64,

    // ── identity (must NOT affect the decision) ──
    pub name: String,
    pub nationality: String,
    pub industry: String,
    /// Mechanism-free flavour text. See the module-level
    /// negative-control invariant: this must never encode a feature
    /// value.
    pub narrative: String,
    pub id: String,
}

impl Case {
    /// The canonical worked base case `B` from the design doc
    /// (structural `p_relocate` ≈ 0.954). A deterministic anchor for
    /// unit tests and smoke runs — identity is fully fictional so it can
    /// never contaminate via a recalled real subject.
    pub fn base_example() -> Self {
        Case {
            wealth: 500.0,
            liquid_frac: 0.90,
            mobility_cost: 0.20,
            home_rate: 0.03,
            best_dest_rate: 0.0,
            exit_tax: 0.0,
            enforcement: 0.90,
            name: "Jordan Vale".into(),
            nationality: "Northvian".into(),
            industry: "manufacturing".into(),
            narrative: synthetic_narrative("Jordan Vale", "Northvian", "manufacturing"),
            id: "mf-base-example".into(),
        }
    }

    /// Return a copy with a fresh synthetic identity but identical
    /// mechanism features — the transformation behind the INV (identity
    /// invariance) test. The new identity is drawn from the same
    /// fictional pools, so it can never introduce a recalled real
    /// subject, and every feature is preserved so the structural prior
    /// is unchanged by construction.
    pub fn swap_identity(&self, rng: &mut StdRng) -> Case {
        let first = pick(rng, FIRST_NAMES);
        let last = pick(rng, LAST_NAMES);
        let name = format!("{first} {last}");
        let nationality = pick(rng, NATIONALITIES).to_string();
        let industry = pick(rng, INDUSTRIES).to_string();
        let narrative = synthetic_narrative(&name, &nationality, &industry);
        Case {
            name,
            nationality,
            industry,
            narrative,
            id: format!("{}~inv", self.id),
            ..self.clone()
        }
    }
}

/// Build the mechanism-free narrative. Centralised so every generated
/// case (and the anchor) shares the same neutral shape.
fn synthetic_narrative(name: &str, nationality: &str, industry: &str) -> String {
    format!("{name} is a {nationality} figure in {industry}, frequently profiled in the financial press.")
}

// ── Synthetic identity pools ─────────────────────────────────────────
//
// Invented given/family names — never real public figures, so an
// identity swap (the INV test) cannot smuggle in a memorized outcome.
// Nationalities are plausible adjectives (kept slightly off-world to
// avoid pinning any real individual); industries are generic sectors.

const FIRST_NAMES: &[&str] = &[
    "Jordan", "Mara", "Tomas", "Elise", "Rafael", "Nadia", "Soren", "Petra", "Idris", "Cora",
    "Lukas", "Ingrid", "Camille", "Anders", "Yara", "Dmitri", "Selene", "Bastian", "Noor", "Theo",
];

const LAST_NAMES: &[&str] = &[
    "Vale", "Okonkwo", "Hargrove", "Lindqvist", "Marchetti", "Devereux", "Halloran", "Beaumont",
    "Castellan", "Voss", "Aaltonen", "Rhodes", "Saint-Clair", "Norrington", "Falk", "Greaves",
    "Underwood", "Calloway", "Brandt", "Pelletier",
];

const NATIONALITIES: &[&str] = &[
    "Northvian", "Solsebran", "Aurelian", "Kestrelander", "Marivanthe", "Hallendish", "Verdene",
    "Castovian", "Brunhild", "Tessaran",
];

const INDUSTRIES: &[&str] = &[
    "manufacturing",
    "asset management",
    "real estate",
    "technology",
    "shipping and logistics",
    "pharmaceuticals",
    "consumer retail",
    "energy",
];

fn pick<'a>(rng: &mut StdRng, xs: &'a [&'a str]) -> &'a str {
    xs[rng.random_range(0..xs.len())]
}

/// Generate `n` synthetic cases deterministically from `seed`.
///
/// Feature ranges are biased so most base cases *lean* toward relocation
/// (positive net incentive): destinations are weakly-to-strongly cheaper
/// than home, exit taxes are modest, enforcement is non-trivial, and
/// base mobility cost is low. That bias is intentional — it gives the
/// DIR-P1 ("exit suddenly becomes expensive") perturbation a real
/// structural collapse to detect on a meaningful fraction of cases,
/// which is where the magnitude band has power.
pub fn generate_cases(n: usize, seed: u64) -> Vec<Case> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n).map(|i| gen_one(&mut rng, seed, i)).collect()
}

fn gen_one(rng: &mut StdRng, seed: u64, idx: usize) -> Case {
    let wealth = rng.random_range(50.0..2000.0);
    let liquid_frac = rng.random_range(0.40..0.98);
    let mobility_cost = rng.random_range(0.05..0.50);
    let home_rate = rng.random_range(0.02..0.05);
    // Destination at or below home — a positive-or-zero differential.
    let best_dest_rate = rng.random_range(0.0..home_rate);
    let exit_tax = rng.random_range(0.0..0.15);
    let enforcement = rng.random_range(0.5..1.0);

    let first = pick(rng, FIRST_NAMES);
    let last = pick(rng, LAST_NAMES);
    let name = format!("{first} {last}");
    let nationality = pick(rng, NATIONALITIES).to_string();
    let industry = pick(rng, INDUSTRIES).to_string();
    let narrative = synthetic_narrative(&name, &nationality, &industry);
    let id = format!("mf-{seed}-{idx:04}");

    Case {
        wealth,
        liquid_frac,
        mobility_cost,
        home_rate,
        best_dest_rate,
        exit_tax,
        enforcement,
        name,
        nationality,
        industry,
        narrative,
        id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic_in_seed() {
        let a = generate_cases(50, 7);
        let b = generate_cases(50, 7);
        assert_eq!(a, b, "same seed must reproduce the battery bit-for-bit");
        let c = generate_cases(50, 8);
        assert_ne!(a, c, "different seed must produce a different battery");
    }

    #[test]
    fn generated_features_are_in_range() {
        for c in generate_cases(500, 42) {
            assert!((50.0..2000.0).contains(&c.wealth));
            assert!((0.40..0.98).contains(&c.liquid_frac));
            assert!((0.05..0.50).contains(&c.mobility_cost));
            assert!((0.02..0.05).contains(&c.home_rate));
            assert!(c.best_dest_rate >= 0.0 && c.best_dest_rate <= c.home_rate);
            assert!((0.0..0.15).contains(&c.exit_tax));
            assert!((0.5..1.0).contains(&c.enforcement));
        }
    }

    #[test]
    fn narrative_is_mechanism_free() {
        // The control's validity rests on the narrative never naming a
        // feature. Cheap guard against accidental leakage in edits.
        let banned = [
            "wealth", "liquid", "mobility", "exit tax", "tax rate", "enforcement", "€", "%",
        ];
        for c in generate_cases(200, 1) {
            let low = c.narrative.to_lowercase();
            for needle in banned {
                assert!(
                    !low.contains(needle),
                    "narrative leaked a mechanism term {needle:?}: {:?}",
                    c.narrative
                );
            }
        }
    }
}
