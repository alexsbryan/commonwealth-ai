// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus-index` — the retrieval read-port leaf.
//!
//! The read surface the host and the engine both reach DOWN to: the shape of
//! an index directory, the hybrid search over it, and the rows it persists.
//! Carved out of `corpus-engine` by domains `REVIEW-build-index-read-port`
//! (DE "The read-port leaf, measured again"; DT retrieval cluster dest) so a
//! thin host can open an index by corpus id without linking the engine that
//! produced it.
//!
//! Modules:
//!
//! - [`index`] — `CorpusIndex`, `IndexMeta`, `IndexInfo`-shaped search and the
//!   evidence/provenance read surface.
//! - [`types`] — the index row types (`IndexInfo`, `ScoredChunk`,
//!   `RerankConfig`, `EmbedFn`, …).
//! - [`error`] — the engine's `Error`/`Result`, moved whole so every `?` and
//!   every `From<corpus_engine::Error>` in the monorepo keeps one type
//!   identity.
//! - [`corpus`] — `Corpus`, "which corpus, and where does it live".
//! - [`recipe`] — the persisted setting types the recipe embeds
//!   (`DisplayMeta`, `MutableMergePolicy`).
//! - [`filters`] — `FilterConfig` / `ComposeMode` and the two per-filter
//!   configs.
//! - [`stream_axes`] — the per-corpus stability metadata.
//! - [`chunkers`] — `CommittedChunk`, the `{id, content_hash}` row.
//!
//! The engine re-exports every item at its historical path, so no importer in
//! the monorepo changed.

pub mod chunkers;
pub mod corpus;
pub mod error;
pub mod filters;
pub mod index;
pub mod recipe;
pub mod stream_axes;
pub mod types;

pub use error::{Error, Result};
