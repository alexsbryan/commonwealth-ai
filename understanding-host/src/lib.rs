// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ports and the knot.
//!
//! Understanding's host tier (DE "Step 7, redrawn", "The shape"): the
//! capabilities with I/O — stores and the ANN, writers, the field engine,
//! entity extraction, the phase runner. It may name `corpus-engine`, the index
//! leaf (`corpus-index`), the pure tier (`understanding-atlas`) and the
//! language (`understanding-vocab`).
//!
//! `REVIEW-build-understanding-crate-tree` wired the tier's `Cargo.toml` from
//! the files it will hold and named `corpus-engine`, which turns the package's
//! grandfathered `[[exception]]` (quality/ARCH_LAYERS.toml:1388-1393) from
//! STALE-by-construction to LIVE. The batch move rows below fill the modules.

#[cfg(test)]
mod tests {
    /// The scaffolding is a claim: the host tier's ONE grandfathered edge
    /// (`understanding-host -> corpus-engine`) is declared and resolves, so
    /// boundary-gate reads the exception as LIVE rather than STALE. Failing
    /// input: the `corpus-engine` dependency dropped from `Cargo.toml`.
    #[test]
    fn the_corpus_engine_exception_resolves() {
        let _corpus: Option<corpus_engine::Corpus> = None;
        let _policies: Option<understanding_vocab::ontology::OntologyPolicies> = None;
    }
}
