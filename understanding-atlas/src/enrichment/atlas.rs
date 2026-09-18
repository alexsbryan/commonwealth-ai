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

// The name-fold (`fold`) was defined in corpus-engine's HOST
// `enrichment/atlas/resolution.rs`; pure files reached it through this module
// (`crate::enrichment::atlas::fold`). It is arithmetic, so it moved here —
// `dm-understanding-pure-1`, ralph/DECISIONS.md.
pub mod fold;
pub use fold::fold;

// The read door's atom reader (`read_atlas_atoms`), re-exported at the
// historical `crate::enrichment::atlas` path so a moved pure file keeps
// resolving it. It already lives in the language (`understanding_vocab::read`).
pub use understanding_vocab::read::read_atlas_atoms;

pub use atoms::{
    AtomEnvelope, AtomId, AtomType, AtomsFile, ChunkRef, Claim, Configuration, Entity, Event,
    Question, Relation, ResolutionStatus, SectionPosition, SectionRange, State,
};
pub use edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
pub use stable_key::StableAtomKey;
