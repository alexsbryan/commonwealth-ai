// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reference class: relocation under a wealth tax. A thin
//! `ReasoningClass` wrapper over the existing (test-covered) `case`,
//! `perturb`, and `structural` modules — the science is unchanged; this
//! just presents it through the generic class seam.

use std::path::Path;

use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::mechanism_fidelity::case::generate_cases;
use crate::mechanism_fidelity::class::{prob_of, ReasoningClass, RenderedProbe};
use crate::mechanism_fidelity::perturb::{render_prompt, RenderMode, Variant};
use crate::mechanism_fidelity::structural::structural_p_relocate;

#[derive(Debug, Default)]
pub struct WealthTaxRelocation;

impl WealthTaxRelocation {
    /// Append the class's letter legend to a rendered case prompt. Keeping
    /// the anchoring here (not in the orchestrator) lets each class own its
    /// own choice set.
    fn anchor(prompt: String) -> String {
        format!("{prompt}\n\nAnswer with exactly one letter — A = relocate, B = stay, C = indifferent.")
    }

    /// Per-(case, variant) deterministic RNG seed for the identity swap,
    /// matching the original orchestrator so a `(seed, n)` battery is
    /// reproducible bit-for-bit.
    fn swap_seed(seed: u64, case_idx: usize, v: Variant) -> u64 {
        seed.wrapping_mul(1_000_003)
            .wrapping_add(case_idx as u64)
            .wrapping_add(v as u64 * 97)
    }
}

impl ReasoningClass for WealthTaxRelocation {
    fn id(&self) -> &'static str {
        "wealth_tax_relocation"
    }

    fn system_prompt(&self) -> &'static str {
        // Byte-stable with the original orchestrator's elicitation system
        // message — the §5 oracle was captured under this exact wording.
        "You are a careful economic analyst. Answer with a single letter."
    }

    fn candidates(&self) -> Vec<String> {
        vec!["A".into(), "B".into(), "C".into()]
    }

    fn target_prob(&self, dist: &[(String, f64)]) -> f64 {
        // A = relocate, B = stay, C = indifferent.
        prob_of(dist, "A") + 0.5 * prob_of(dist, "C")
    }

    fn build_probes(&self, n: usize, seed: u64, _corpus: Option<&Path>) -> Vec<RenderedProbe> {
        let cases = generate_cases(n, seed);
        let full_variants = Variant::all();
        let control_variants = [Variant::Base, Variant::DirP1, Variant::DirP2];

        let mut out = Vec::new();
        for (ci, base) in cases.iter().enumerate() {
            // Precompute the (perturbed case, structural p) per variant once.
            let mut per_variant = Vec::with_capacity(full_variants.len());
            for &v in &full_variants {
                let mut rng = StdRng::seed_from_u64(Self::swap_seed(seed, ci, v));
                let c = v.apply(base, &mut rng);
                let sp = structural_p_relocate(&c);
                per_variant.push((v, c, sp));
            }
            let lookup = |v: Variant| per_variant.iter().find(|(pv, _, _)| *pv == v).unwrap();

            let mut push = |v: Variant, mode: RenderMode, paraphrase: bool| {
                let (_, c, sp) = lookup(v);
                out.push(RenderedProbe {
                    case_id: base.id.clone(),
                    variant: v.label().to_string(),
                    render: mode.label().to_string(),
                    paraphrase,
                    kind: v.kind(),
                    expected_sign: v.expected_sign(),
                    prompt: Self::anchor(render_prompt(c, mode, paraphrase)),
                    structural_p: *sp,
                });
            };

            // Full render, primary wording (base first).
            for &v in &full_variants {
                push(v, RenderMode::Full, false);
            }
            // Full render, paraphrase wording.
            for &v in &full_variants {
                push(v, RenderMode::Full, true);
            }
            // Stripped render — the negative control (DIR variants + base).
            for &v in &control_variants {
                push(v, RenderMode::Stripped, false);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_probes_shape_and_oracle() {
        let cls = WealthTaxRelocation;
        // 2 cases × (full×4 + full-para×4 + control×3) = 22 probes.
        let probes = cls.build_probes(2, 0, None);
        assert_eq!(probes.len(), 22);

        // Base-first within each context (the scorer needs the reference).
        assert!(probes[0].is_base() && probes[0].render == "full" && !probes[0].paraphrase);

        // The control's DIR perturbations carry the structural delta even
        // though they're rendered feature-blind.
        let base_full = probes
            .iter()
            .find(|p| p.is_base() && p.render == "full" && !p.paraphrase)
            .unwrap();
        let p1_full = probes
            .iter()
            .find(|p| p.variant == "dir_p1" && p.render == "full" && !p.paraphrase)
            .unwrap();
        assert!(
            p1_full.structural_p < base_full.structural_p - 0.5,
            "P1 must carry a large structural collapse"
        );

        // The letter legend is anchored into every prompt.
        assert!(probes[0].prompt.contains("A = relocate"));
        // Stripped control hides the feature block.
        let ctrl = probes.iter().find(|p| p.is_control()).unwrap();
        assert!(!ctrl.prompt.contains("wealth-tax rate"));
    }

    #[test]
    fn target_prob_maps_relocate_and_indifferent() {
        let cls = WealthTaxRelocation;
        let dist = vec![("A".into(), 0.6), ("B".into(), 0.3), ("C".into(), 0.1)];
        assert!((cls.target_prob(&dist) - (0.6 + 0.05)).abs() < 1e-9);
    }
}
