// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Corpus`] — one installed corpus, named and located — now DEFINED in the
//! `corpus-index` leaf.
//!
//! `Corpus` is "which corpus, and where does it live", which is the index
//! directory's own shape, so it moved with the read-port carve (domains
//! `REVIEW-build-index-read-port`) and is re-exported here at its historical
//! `crate::corpus::*` path.

pub use corpus_index::corpus::*; // shim: moved by domains REVIEW-build-index-read-port
