// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas query traversal + brief assembly — the pure half.
//!
//! `REVIEW-build-understanding-crate-tree` decided the module tree; this is
//! the path-preserving prefix the `dm-understanding-pure-*` batch rows fill.
//! The read side of the v2 enrichment stack: a natural-language query is
//! classified into a traversal plan, the resolved atlas is walked, and a
//! brief is assembled.
//!
//! The pure children (`brief`, `classifier`, `engine`, `spans`) live here;
//! `question_kind` (host — it embeds) stays in the engine's shell and is
//! re-exported at its historical path.

pub mod brief;
pub mod classifier;
pub mod engine;
pub mod spans;

// The declared-ontology fixture the classifier and traversal tests use in
// place of `corpus-engine`'s `recipe_templates::numismatics_policies` (a
// recipe-TOML parser, hence host). Built from language types so the pure tier
// keeps no corpus-engine edge, even in tests.
#[cfg(test)]
pub(crate) mod test_fixtures;
