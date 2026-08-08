// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Fidelity-Flywheel substrate: the shared contracts every signal source
//! (I1–I5) and output channel reuses.
//!
//! The flywheel is the closed control loop by which the system gets measurably
//! better at its moat — grounded-or-abstain, accurate abstention, correct
//! register routing — from corpus + compute (not user traffic), compounding the
//! *scaffolding* and never the base-model weights. This module is the
//! substrate; the live adapter (driving the chat path, running the forced-choice
//! judges) and the promotion controller live in `sovereign-cli-llm`'s
//! `bench_cmd`, mirroring how chaos-monkey splits pure logic from live adapter.
//!
//! Contracts:
//! - [`probe::Probe`] — the unified "generate" output (+ [`probe::Oracle`]).
//! - [`verify::Verifier`]-style [`verify::DeterministicVerifier`] +
//!   [`verify::FailureClass`] — the unified "verify" output (the five-way
//!   failure taxonomy).
//! - [`case::RegressionCase`] / [`case::RegressionBank`] — durable, replayable
//!   failures (G3), fairness-validated at capture AND load.
//! - [`Generator`] — the plug-in seam: a new signal source is a new `Generator`
//!   impl + one line in [`registry`], with ZERO change to verify / score /
//!   capture / gate. That asymmetry (open for extension, closed for
//!   modification) is the substrate's whole point.
//!
//! Pure logic (serde + std only) so it rebuilds and unit-tests in seconds.

use std::path::Path;

pub mod calibration;
pub mod case;
pub mod det_checks;
pub mod generators;
pub mod mining;
pub mod passages;
pub mod probe;
pub mod redteam;
pub mod verify;

pub use case::{validate_fairness, RegressionBank, RegressionCase};
pub use probe::{chaos_to_probe, AbsentKind, Oracle, Probe, ProbeSource};
pub use verify::{Determinism, DeterministicVerifier, FailureClass, Observation, Verdict};

/// Score a set of verdicts into the two-red-line report — the SAME scorer the
/// chaos bench uses, fed the verdicts' rows. One scorer of record, no drift.
pub fn score(verdicts: &[Verdict]) -> crate::chaos_monkey::CalibrationReport {
    let rows: Vec<_> = verdicts.iter().map(|v| v.row.clone()).collect();
    crate::chaos_monkey::score(&rows)
}

/// The plug-in seam. I1–I5 each implement this and register in [`registry`].
pub trait Generator {
    /// Stable id, e.g. `"i1_corpus"`.
    fn id(&self) -> &'static str;
    /// Emit up to `n` probes mined from `corpus` (the indexed corpus root,
    /// mined like attribution mines `atlas/atoms.json`). `(n, seed)` is
    /// reproducible bit-for-bit so a battery is replayable.
    fn generate(&self, n: usize, seed: u64, corpus: Option<&Path>) -> Vec<Probe>;
}

/// The registered generators. Adding a signal source is one line here.
pub fn registry() -> Vec<Box<dyn Generator>> {
    vec![
        Box::new(generators::corpus::CorpusGenerator::default()),
        Box::new(generators::adversarial::AdversarialGenerator),
    ]
}

/// Resolve a generator by id.
pub fn by_id(id: &str) -> Option<Box<dyn Generator>> {
    registry().into_iter().find(|g| g.id() == id)
}

/// All registered generator ids (for `--help` / listing).
pub fn generator_ids() -> Vec<&'static str> {
    registry().iter().map(|g| g.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_unique_and_resolvable() {
        let ids = generator_ids();
        assert!(ids.contains(&"i1_corpus"));
        assert!(ids.contains(&"i2_adversarial"));
        let mut seen = std::collections::HashSet::new();
        for id in &ids {
            assert!(seen.insert(*id), "duplicate generator id {id}");
            assert!(by_id(id).is_some(), "{id} must resolve");
        }
        assert!(by_id("nope").is_none());
    }
}
