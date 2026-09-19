// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure ontology surface, split out of `corpus-engine`'s HOST
//! `enrichment/ontology/mod.rs`.
//!
//! `REVIEW-build-understanding-crate-tree` moved the two pure children
//! (`clock`, `type_index`) and the language re-exports here; the engine's
//! shell keeps `validate` and the `recipe_ontology::language` re-exports,
//! which parse recipe TOML and so are host. The re-export list is the same one
//! the shell carried, so a moved file's `crate::enrichment::ontology::{
//! OntologyPolicies, TypeIndex, TypeKind, …}` reach keeps resolving.

pub mod clock;
pub mod type_index;

pub use clock::section_date;
pub use type_index::TypeIndex;

// The parsed policy data and the author-facing declaration types — the leaf.
pub use understanding_vocab::ontology::decl::{
    AttrDecl, AttrFamily, ChangeDecl, ClaimScopeDecl, Deontic, DeriveDecl, Force, OntologyTypeDecl,
    OntologyV1, OntologyVocabulary, SourceDecl, SupersessionClock, TensionDecl, TypeKind,
    VoicesDecl,
};
pub use understanding_vocab::ontology::{
    AssertionPolicy, ChangePolicy, DerivationPolicy, IdentityPolicy, NavigationPolicy,
    OntologyPolicies, ProsePolicy, QuestionKind, SeedPolicy, ShapePolicy, WalkPolicy,
};
