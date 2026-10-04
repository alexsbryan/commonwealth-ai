// SPDX-License-Identifier: AGPL-3.0-or-later
//! `index` — the retrieval read-port surface, now DEFINED in the `corpus-index`
//! leaf and re-exported here at its historical `crate::index::*` paths.
//!
//! The six clean files (`search`, `create`, `write`, `maintain`, `evidence`,
//! `provenance`), `read` and the `CorpusIndex`/`IndexMeta` part of the old
//! `mod.rs` moved with the read-port carve (domains
//! `REVIEW-build-index-read-port`, DE "The read-port leaf, measured again").
//! What stays is host code that is not part of the leaf: [`field_skeleton`]
//! (JSON IO over an index directory) and [`raptor`] (the RAPTOR summary ANN
//! table, which names `crate::enrichment`/`crate::atlas_context`).

pub mod field_skeleton;
pub mod raptor;

pub use corpus_index::index::*; // shim: moved by domains REVIEW-build-index-read-port
