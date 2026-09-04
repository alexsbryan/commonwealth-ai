// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology — the enrichment side of a recipe's `[enrichment.ontology]`.
//!
//! Since 2026-09-03 (enrichment-as-plugin Step 3) this module OWNS only what
//! reads policies: the type index, the section clock, and `recipe validate`'s
//! ontology rules. The two halves it used to hold moved, each to the side
//! that owns it, and are re-exported here so no path changed:
//!
//! - The parsed policy DATA — [`OntologyPolicies`] and its five axes plus
//!   prose, and the declaration types an author writes (`OntologyTypeDecl`,
//!   `TypeKind`, `AttrDecl`, …, `OntologyV1`) — live in the
//!   `corpus-engine-vocab` leaf (`corpus_engine_vocab::ontology`). They
//!   derive `Deserialize` for the JSON round trip an atlas records
//!   (`atlas/ontology.json`), so a thin host can read what a corpus declared
//!   without linking this crate.
//! - The declaration LANGUAGES — [`OntologyLanguage`], its registry, V0 and
//!   V1 — parse recipe TOML, which makes them recipe parsing; they live in
//!   [`crate::recipe_ontology::language`]. Before the move `recipe.rs` and
//!   this module imported each other (`recipe_ontology::OntologyBlock::
//!   policies` called the registry here; `language.rs` imported the recipe's
//!   decl types), a genuine cycle only a shared leaf could break.
//!
//! Direction now: `recipe_ontology` → leaf; `enrichment::ontology` → recipe +
//! leaf. The one recipe → enrichment edge left in the ontology area is
//! `recipe_parsing::check_ontology_block`'s use of `validate`-side rules, and
//! that is the load gate doing its job.
//!
//! Five axes plus prose (`ONTOLOGY_PRIMITIVES.md` §0.1, §2): what exists
//! (shape), what is said (assertion), what is the same (identity), what
//! changes (change), what follows (derivation). The composer, parser,
//! resolver, tension selector, supersession fold, build report and inspector
//! read [`OntologyPolicies`] and nothing else; the recipe's `version` selects
//! an [`OntologyLanguage`] that parses TOML into these structs and is never
//! consulted again. That is what makes a version 2 cheap.

pub mod clock;
pub mod type_index;
mod validate;

pub use clock::section_date;
pub use type_index::TypeIndex;
pub use validate::{
    validate_block, OntologyValidation, MAX_ATTRS_PER_TYPE, MAX_ENUM_VALUES, MAX_TYPES_PER_KIND,
    RESERVED_CLAIM_KINDS,
};

// The declaration languages — recipe parsing, so they live with the recipe.
pub use crate::recipe_ontology::language;
pub use crate::recipe_ontology::language::{OntologyLanguage, OntologyLanguageRegistry};

// The parsed policy data and the author-facing declaration types — the leaf.
pub use corpus_engine_vocab::ontology::decl::{
    AttrDecl, AttrFamily, ChangeDecl, ClaimScopeDecl, Deontic, DeriveDecl, Force, OntologyTypeDecl,
    OntologyV1, SourceDecl, SupersessionClock, TensionDecl, TypeKind, VoicesDecl,
};
pub use corpus_engine_vocab::ontology::{
    AssertionPolicy, ChangePolicy, DerivationPolicy, IdentityPolicy, OntologyPolicies, ProsePolicy,
    ShapePolicy,
};
