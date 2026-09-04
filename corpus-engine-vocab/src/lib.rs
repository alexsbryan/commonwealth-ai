// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus-engine-vocab` — the atlas vocabulary, as a leaf.
//!
//! What an enrichment PRODUCES, separated from what produces it. Four
//! modules, every one of them data:
//!
//! - [`atoms`] — `AtomsFile` / `AtomEnvelope` and the eleven atom kinds
//!   (Entity, Event, State, Relation, Claim, Question, Configuration,
//!   ArgumentReconstruction, Position, Opposition, Asset); the on-disk
//!   shape of `atlas/atoms.json`.
//! - [`edges`] — `Edge` / `EdgesFile`; the shape of `atlas/edges.json`.
//! - [`taxonomy`] — `EnrichmentDepth` and the kind vocabularies every atom
//!   names.
//! - [`ontology`] — `OntologyPolicies` (five axes + prose) and, under
//!   [`ontology::decl`], everything an author declares: `OntologyTypeDecl`,
//!   `TypeKind`, `AttrDecl`, `OntologyV1`, the investigation
//!   `EntityTypeDecl` / `RelationshipTypeDecl` / `PatternDecl`, and
//!   `OntologyVocabulary`; the shape of `atlas/ontology.json`.
//! - [`canonical`] — `lookup_key`, the canonical-name fold both the
//!   resolver and retrieval-time lookups use.
//! - [`stable_key`] — `StableAtomKey`, an atom's content-derived identity
//!   (ARCH §7.5). It reads only atom fields, and an inherent `impl
//!   AtomEnvelope` has to live where `AtomEnvelope` does.
//!
//! Closure: `serde`, `serde_json`, `kernel-types`, `blake3` (the stable
//! key's hash — changing it would re-key every curation overlay). No IO,
//! no features. A
//! consumer that wants to name an atom links this; a consumer that wants
//! to MAKE one links `corpus-engine`.

pub mod atoms;
pub mod canonical;
pub mod edges;
pub mod ontology;
pub mod stable_key;
pub mod taxonomy;
