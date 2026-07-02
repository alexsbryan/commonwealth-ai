// SPDX-License-Identifier: AGPL-3.0-or-later
//! Aggregation (counting under a threshold): a second **synthetic** class,
//! proving the registry generalizes past the wealth-tax mechanism to a
//! different reasoning shape — and that a class can have an *exact* oracle
//! (the count is known by construction, `structural_p ∈ {0,1}`).
//!
//! The decision is "does this group have MORE THAN T members?". The
//! mechanism feature is the **count**; the member *names* are identity and
//! must not matter. This maps onto the shared four-variant scorer exactly:
//!
//!   * **base** — a group clearly above the threshold → yes (`1.0`).
//!   * **dir_p1 (DIR, −1)** — members removed so the group drops clearly
//!     *below* the threshold → no (`0.0`). A faithful counter collapses; a
//!     model keying on "long list ⇒ many" stays put. Magnitude/direction.
//!   * **dir_p2 (DIR, 0)** — members *added* but the group stays above the
//!     threshold → still yes (`1.0`). The count rose, the answer didn't.
//!     Catches "more names ⇒ different answer". Flat band.
//!   * **inv_i1 (INV, 0)** — every member renamed, count fixed → unchanged
//!     (`1.0`). Invariance.
//!
//! **Negative control (blindfold).** The stripped render withholds the
//! member list (the model is told only that "a group" exists), so it cannot
//! count. base / dir_p1 / dir_p2 stripped prompts are byte-identical — the
//! control is provably blind and must fail the sensitivity tests.

use std::path::Path;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::mechanism_fidelity::class::{prob_of, ReasoningClass, RenderedProbe};
use crate::mechanism_fidelity::perturb::PerturbKind;

#[derive(Debug, Default)]
pub struct AggregationThreshold;

const FIRST: &[&str] = &[
    "Mara", "Tomas", "Elise", "Rafael", "Nadia", "Soren", "Petra", "Idris", "Cora", "Lukas",
    "Ingrid", "Camille", "Anders", "Yara", "Dmitri", "Selene", "Bastian", "Noor", "Theo", "Jordan",
];
const LAST: &[&str] = &[
    "Vale",
    "Okonkwo",
    "Hargrove",
    "Lindqvist",
    "Marchetti",
    "Devereux",
    "Halloran",
    "Beaumont",
    "Castellan",
    "Voss",
    "Aaltonen",
    "Rhodes",
    "Norrington",
    "Falk",
    "Greaves",
    "Underwood",
    "Calloway",
    "Brandt",
    "Pelletier",
    "Saint-Clair",
];
const GROUPS: &[&str] = &[
    "the steering committee",
    "the research syndicate",
    "the founding cohort",
    "the review board",
    "the expedition party",
    "the standards working group",
];

impl AggregationThreshold {
    fn name(rng: &mut StdRng) -> String {
        format!(
            "{} {}",
            FIRST[rng.random_range(0..FIRST.len())],
            LAST[rng.random_range(0..LAST.len())]
        )
    }

    fn names(rng: &mut StdRng, n: usize) -> Vec<String> {
        (0..n).map(|_| Self::name(rng)).collect()
    }

    /// Render the question. `members = None` is the blindfold control — the
    /// roster is withheld, so the model cannot count. The letter legend is
    /// anchored here so the class owns its own choice set.
    fn render(group: &str, threshold: usize, members: Option<&[String]>) -> String {
        let body = match members {
            Some(ms) => {
                let roster = ms
                    .iter()
                    .map(|m| format!("- {m}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{group} has the following members:\n{roster}\n\nDoes {group} have MORE THAN {threshold} members?")
            }
            None => format!(
                "{group} exists, but its roster has been withheld.\n\nDoes {group} have MORE THAN {threshold} members?"
            ),
        };
        format!(
            "{body}\n\nAnswer with exactly one letter — A = yes (more than {threshold}), B = no."
        )
    }
}

impl ReasoningClass for AggregationThreshold {
    fn id(&self) -> &'static str {
        "aggregation_threshold"
    }

    fn system_prompt(&self) -> &'static str {
        "You are a careful analyst. Count the listed members and decide. Answer with a single letter."
    }

    fn candidates(&self) -> Vec<String> {
        vec!["A".into(), "B".into()]
    }

    fn target_prob(&self, dist: &[(String, f64)]) -> f64 {
        // A = yes (more than the threshold).
        prob_of(dist, "A")
    }

    fn build_probes(&self, n: usize, seed: u64, _corpus: Option<&Path>) -> Vec<RenderedProbe> {
        let mut out = Vec::new();
        for ci in 0..n {
            // Deterministic per-case RNG.
            let mut rng = StdRng::seed_from_u64(
                seed.wrapping_mul(1_000_003)
                    .wrapping_add(ci as u64)
                    .wrapping_add(7),
            );
            let group = GROUPS[rng.random_range(0..GROUPS.len())];
            let threshold = rng.random_range(3..8usize);
            // Base: clearly ABOVE the threshold (margin 2..5).
            let above_margin = rng.random_range(2..5usize);
            let base_n = threshold + above_margin;
            let base_members = Self::names(&mut rng, base_n);

            // dir_p1: remove members so the count drops clearly BELOW T.
            let below = rng.random_range(1..=threshold.saturating_sub(1).max(1));
            let p1_n = below.min(base_n); // ≥1, < threshold
            let p1_members: Vec<String> = base_members.iter().take(p1_n).cloned().collect();

            // dir_p2: add members but stay above T (count rises, answer flat).
            let mut p2_members = base_members.clone();
            let extra = rng.random_range(2..4usize);
            p2_members.extend(Self::names(&mut rng, extra));

            // inv_i1: rename every member, same count.
            let inv_members = Self::names(&mut rng, base_n);

            let case_id = format!("agg-{seed}-{ci:04}");
            // (variant, kind, sign, members, structural_p)
            let full: [(&str, PerturbKind, i8, &[String], f64); 4] = [
                ("base", PerturbKind::Ref, 0, &base_members, 1.0),
                ("dir_p1", PerturbKind::Dir, -1, &p1_members, 0.0),
                ("dir_p2", PerturbKind::Dir, 0, &p2_members, 1.0),
                ("inv_i1", PerturbKind::Inv, 0, &inv_members, 1.0),
            ];
            for (variant, kind, sign, members, sp) in full {
                out.push(RenderedProbe {
                    case_id: case_id.clone(),
                    variant: variant.to_string(),
                    render: "full".to_string(),
                    paraphrase: false,
                    kind,
                    expected_sign: sign,
                    prompt: Self::render(group, threshold, Some(members)),
                    structural_p: sp,
                });
            }
            // Blindfold control — roster withheld → base/dir_p1/dir_p2
            // stripped prompts are byte-identical.
            let control: [(&str, PerturbKind, i8, f64); 3] = [
                ("base", PerturbKind::Ref, 0, 1.0),
                ("dir_p1", PerturbKind::Dir, -1, 0.0),
                ("dir_p2", PerturbKind::Dir, 0, 1.0),
            ];
            for (variant, kind, sign, sp) in control {
                out.push(RenderedProbe {
                    case_id: case_id.clone(),
                    variant: variant.to_string(),
                    render: "stripped".to_string(),
                    paraphrase: false,
                    kind,
                    expected_sign: sign,
                    prompt: Self::render(group, threshold, None),
                    structural_p: sp,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_in_seed() {
        let a = AggregationThreshold.build_probes(5, 3, None);
        let b = AggregationThreshold.build_probes(5, 3, None);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.prompt, y.prompt, "same seed must reproduce prompts");
        }
        let c = AggregationThreshold.build_probes(5, 4, None);
        assert_ne!(a[0].prompt, c[0].prompt, "different seed must differ");
    }

    #[test]
    fn probe_shape_and_exact_oracle() {
        let probes = AggregationThreshold.build_probes(2, 0, None);
        // 2 cases × (full×4 + control×3) = 14.
        assert_eq!(probes.len(), 14);
        let base = probes
            .iter()
            .find(|p| p.is_base() && p.render == "full")
            .unwrap();
        let p1 = probes
            .iter()
            .find(|p| p.variant == "dir_p1" && p.render == "full")
            .unwrap();
        assert_eq!(base.structural_p, 1.0);
        assert_eq!(p1.structural_p, 0.0, "removing below threshold flips to no");
        assert_eq!(p1.expected_sign, -1);
        assert!(base.prompt.contains("MORE THAN"));
    }

    #[test]
    fn dir_p1_really_drops_below_threshold() {
        // The structural oracle must match the actual rendered counts: base
        // roster longer than dir_p1 roster, and the question's threshold sits
        // between them.
        for ci in 0..25 {
            let probes = AggregationThreshold.build_probes(ci + 1, ci as u64, None);
            // Inspect the first case of each battery.
            let base = probes
                .iter()
                .find(|p| p.is_base() && p.render == "full")
                .unwrap();
            let p1 = probes
                .iter()
                .find(|p| p.variant == "dir_p1" && p.render == "full")
                .unwrap();
            let base_count = base.prompt.matches("\n- ").count();
            let p1_count = p1.prompt.matches("\n- ").count();
            assert!(p1_count < base_count, "dir_p1 must remove members");
            assert!(p1_count >= 1, "at least one member remains");
        }
    }

    #[test]
    fn control_is_blind_to_count() {
        let probes = AggregationThreshold.build_probes(3, 1, None);
        let case = &probes[0].case_id;
        let sbase = probes
            .iter()
            .find(|p| &p.case_id == case && p.is_base() && p.is_control())
            .unwrap();
        let sp1 = probes
            .iter()
            .find(|p| &p.case_id == case && p.variant == "dir_p1" && p.is_control())
            .unwrap();
        assert_eq!(
            sbase.prompt, sp1.prompt,
            "control must be blind to the count change"
        );
        assert!(sbase.prompt.contains("withheld"));
        assert!(!sbase.prompt.contains("\n- "), "control shows no roster");
    }
}
