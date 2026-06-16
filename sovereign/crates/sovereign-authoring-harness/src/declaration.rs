// SPDX-License-Identifier: AGPL-3.0-or-later
//! Declared thresholds, read off the recipe / harness config and printed into
//! `Verdict.expected` so the bar is always on screen (I5). No DSL — a plain
//! struct of values.

/// Per-run thresholds. Today just the field-coverage floor; the chunk bound and
/// embed model are read directly off the stage outputs, so they need no entry
/// here.
#[derive(Debug, Clone)]
pub struct Declaration {
    /// Fraction of docs (or source files, for section extractors) a declared
    /// field must cover to pass. Default `1.0` = present-in-all.
    pub min_coverage: f64,
}

impl Default for Declaration {
    fn default() -> Self {
        Self { min_coverage: 1.0 }
    }
}
