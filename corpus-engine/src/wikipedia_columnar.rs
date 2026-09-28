// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wiki link graph's reader and wiki-class walk provider live in the
//! `corpus-engine-atlas-reader` leaf since pb-corpus-mcp-reads (FIVE_PROGRAMS
//! §12 decision 1: the leaf holds RAW reads); re-exported here at the
//! historical path. The tests stay here because they build the store with
//! this crate's writer (`enrichment::atlas::wiki_store`).

pub use corpus_engine_atlas_reader::wikipedia_columnar::*;

#[cfg(test)]
mod tests;
