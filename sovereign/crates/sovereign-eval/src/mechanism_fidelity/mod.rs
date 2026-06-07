//! Mechanism-Fidelity Validation Harness — pure logic.
//!
//! Decides, per policy mechanism, whether a frozen LLM agent reasons
//! from the *causal mechanism* or from *memorized association with the
//! label*. The framing is metamorphic testing: we cannot score the
//! agent against an oracle, so we check relations that must hold under
//! input transformations — **invariance** to identity-preserving changes
//! and **directional responsiveness** to mechanism-feature changes —
//! measured only on synthetic cases the model cannot have memorized.
//!
//! This module is mechanism-agnostic in shape but ships one reference
//! mechanism: relocation under a wealth tax. Adding a mechanism means
//! supplying a [`case`] schema + generator, a [`structural`] prior, and
//! a [`perturb`] set; [`score`] and the pool discipline are unchanged.
//!
//! Everything here is pure (no inference, no I/O beyond serde) so it
//! rebuilds and unit-tests in seconds. The elicitation adapter,
//! mesh fan-out, and pool gating live in the `sovereign-cli-llm`
//! `bench_cmd` orchestrator, which reuses
//! [`crate::entity_resolution_bench::PeekBudget`] for the sacred-test
//! burn-down.
//!
//! **Honest boundary:** the synthetic loop measures mechanism
//! *consistency* (agreement with the structural prior) and *instrument
//! validity* (the negative control fails while a sanity agent passes).
//! It does NOT measure correctness — only a real, scarce holdout tests
//! correspondence to reality. Nothing here should be read as the agent
//! getting closer to truth.

pub mod card;
pub mod case;
pub mod class;
pub mod classes;
pub mod perturb;
pub mod registry;
pub mod score;
pub mod stopping;
pub mod structural;

pub use card::{grade_class, CardEntry, FidelityCard, Grade, GradeThresholds};
pub use case::{generate_cases, Case};
pub use class::{prob_of, ReasoningClass, RenderedProbe};
pub use perturb::{render_prompt, PerturbKind, RenderMode, Variant};
pub use registry::{by_id, class_ids, registry};
pub use score::{score, Bands, ResultRow, Scores};
pub use stopping::{decide, decide_at, BoundedMean, Side, StoppingConfig, Verdict};
pub use structural::structural_p_relocate;

use serde::{Deserialize, Serialize};

/// The three-pool discipline. The spine of the design: synthetic pools
/// are infinitely refreshable (regenerate every batch to outrun adaptive
/// overfitting); the real holdout is scarce, append-only, and sacred.
///
/// Mirrors the semantics of
/// [`crate::entity_resolution_bench::Split`] but names the pools in the
/// design doc's vocabulary. [`Pool::Test`] is the sacred real pool — the
/// analogue of that module's `Holdout` — and must be unsealed
/// explicitly, burning a [`crate::entity_resolution_bench::PeekBudget`]
/// counter when it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pool {
    /// Synthetic, infinite. Generated cases + perturbations labeled by
    /// the structural prior. Regenerated every batch.
    Train,
    /// Synthetic, refreshable. Immediate feedback for scaffolding
    /// choices, with the negative control run every batch.
    Dev,
    /// Real, scarce, append-only. Natural experiments + post-cutoff
    /// events. Queried rarely through the peek budget. The ONLY signal
    /// of correspondence to reality — never an optimization target.
    Test,
}

impl Pool {
    pub fn as_str(&self) -> &'static str {
        match self {
            Pool::Train => "train",
            Pool::Dev => "dev",
            Pool::Test => "test",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "train" => Some(Pool::Train),
            "dev" => Some(Pool::Dev),
            "test" => Some(Pool::Test),
            _ => None,
        }
    }

    /// The sacred real pool refuses to run without an explicit unseal,
    /// exactly as the entity-resolution holdout does.
    pub fn requires_unseal(&self) -> bool {
        matches!(self, Pool::Test)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_round_trips_and_gates() {
        for p in [Pool::Train, Pool::Dev, Pool::Test] {
            assert_eq!(Pool::parse(p.as_str()), Some(p));
        }
        assert!(!Pool::Train.requires_unseal());
        assert!(!Pool::Dev.requires_unseal());
        assert!(Pool::Test.requires_unseal());
        assert_eq!(Pool::parse("nonsense"), None);
    }
}
