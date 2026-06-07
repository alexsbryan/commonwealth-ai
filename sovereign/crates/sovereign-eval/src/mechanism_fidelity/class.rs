//! The `ReasoningClass` abstraction — the seam that turns the
//! wealth-tax-specific harness into a registry of reasoning-fidelity tests.
//!
//! A *class* bundles everything class-specific: the forced-choice label
//! set, how to read a target probability off the distribution, and how to
//! build its probe matrix (mined from a corpus, or synthetic). The
//! orchestrator (elicitation, scoring, pools, early-stopping, cards) and
//! the scorer (`Bands`/`score`) are all generic over the class — adding a
//! class is implementing this trait + registering it, nothing more.
//!
//! A class emits a flat list of [`RenderedProbe`]s. Each carries the
//! finished prompt plus the metadata the scorer needs; the orchestrator
//! groups them by `(case_id, render, paraphrase)` to take base-relative
//! deltas, exactly as the wealth-tax path did, but without knowing
//! anything about wealth taxes.

use std::path::Path;

use super::perturb::PerturbKind;

/// One fully-rendered probe: a prompt to send + the metadata to score it.
///
/// `case_id` is shared by a base case and its perturbations, so the
/// orchestrator can find the reference (`variant == "base"`) within the
/// same `(render, paraphrase)` context and score `d_agent` against it.
#[derive(Debug, Clone)]
pub struct RenderedProbe {
    /// The BASE case id — shared across a base and its perturbations.
    pub case_id: String,
    /// Stable variant label (`"base"`, `"dir_p1"`, …).
    pub variant: String,
    /// `"full"` (features visible) or `"stripped"` (the negative control).
    pub render: String,
    pub paraphrase: bool,
    pub kind: PerturbKind,
    /// Structurally-predicted sign of `p(perturbed) − p(base)`.
    pub expected_sign: i8,
    /// The finished, letter-anchored prompt to elicit.
    pub prompt: String,
    /// The structural prior's probability for THIS probe's (possibly
    /// perturbed) case — the oracle the DIR deltas are scored against.
    pub structural_p: f64,
}

impl RenderedProbe {
    /// True for the feature-stripped negative control.
    pub fn is_control(&self) -> bool {
        self.render == "stripped"
    }

    /// True for the reference probe within its context.
    pub fn is_base(&self) -> bool {
        self.variant == "base"
    }
}

/// A reasoning-fidelity test for one kind of reasoning. The orchestrator
/// drives any implementor generically.
pub trait ReasoningClass: Send + Sync {
    /// Stable id used in `ResultRow.class`, the manifest, and cards.
    fn id(&self) -> &'static str;

    /// The system prompt for elicitation. Kept on the class (not the
    /// orchestrator) so the request is fully determined by the class +
    /// probe — each class frames its task in its own terms, and the
    /// wealth-tax wording stays byte-stable for reproducibility.
    fn system_prompt(&self) -> &'static str;

    /// Forced-choice candidate labels — letter-anchored and single-token
    /// (`["A","B","C"]`, `["A","B"]`) so the one-pass logprob read is clean.
    fn candidates(&self) -> Vec<String>;

    /// Map the forced-choice distribution to the scalar "target"
    /// probability the metamorphic deltas score — e.g. P(relocate) =
    /// P(A) + ½·P(C) for the ternary class, P(supported) = P(A) for a
    /// binary class. `dist` is `(label, probability)` over `candidates()`.
    fn target_prob(&self, dist: &[(String, f64)]) -> f64;

    /// Build the full probe matrix for `n` base cases from `seed`.
    /// `corpus` is the path to an indexed corpus for corpus-grounded
    /// classes (attribution mines `atlas/atoms.json` under it); synthetic
    /// classes (wealth-tax) ignore it.
    fn build_probes(&self, n: usize, seed: u64, corpus: Option<&Path>) -> Vec<RenderedProbe>;
}

/// Look up `dist` for a label, defaulting to 0 — a small helper for
/// `target_prob` implementations.
pub fn prob_of(dist: &[(String, f64)], label: &str) -> f64 {
    dist.iter()
        .find(|(l, _)| l == label)
        .map(|(_, v)| *v)
        .unwrap_or(0.0)
}
