// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `{id, content_hash}` row the index reads back.
//!
//! `CommittedChunk` is the row `CorpusIndex` records for every chunk, so it is
//! DEFINED here and `corpus-engine`'s `chunkers` module re-exports it (DE "The
//! read-port leaf, measured again": "`index/read` names the chunker's
//! `CommittedChunk`").

/// One chunk previously committed to the index, with the
/// content-hash that the index recorded for it. Used by
/// `chunk_delta` to compute what's changed between an old
/// version of a document and its new content.
#[derive(Debug, Clone)]
pub struct CommittedChunk {
    pub id: u64,
    pub content_hash: String,
}
