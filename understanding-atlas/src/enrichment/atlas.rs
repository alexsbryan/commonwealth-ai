// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas vocabulary surface, as `understanding-atlas` reaches it.
//!
//! Split from `corpus-engine`'s HOST `enrichment/atlas/mod.rs` by domains
//! `REVIEW-build-understanding-crate-tree`. The pure half is what the language
//! already owns — `atoms`, `edges` and `stable_key` — re-exported here so a
//! moved pure file's `crate::enrichment::atlas::{AtomEnvelope, atoms::Entity,
//! …}` reach keeps resolving. The host declarations (`ann_store`, `context`,
//! `resolution`, `writer`, `atlas_teardown`, …) stay in the engine's shell;
//! the pure submodules (`axis_catalog`, `citation`, `ground`, `resolve`, …)
//! are declared here by the batch move rows as their files arrive.

pub use understanding_vocab::atoms;
pub use understanding_vocab::edges;
pub use understanding_vocab::stable_key;

pub use atoms::{
    AtomEnvelope, AtomId, AtomType, AtomsFile, ChunkRef, Claim, Configuration, Entity, Event,
    Question, Relation, ResolutionStatus, SectionPosition, SectionRange, State,
};
pub use edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
pub use stable_key::StableAtomKey;
