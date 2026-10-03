// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine's error type, now DEFINED in the `corpus-index` leaf.
//!
//! `Error`/`Result` moved whole with the read-port carve (domains
//! `REVIEW-build-index-read-port`) so every `?` site and every external
//! `From<corpus_engine::Error>` keeps ONE type identity — the leaf re-exports
//! it here at the historical `crate::error::*` path. The one thing that could
//! not travel is the feature-gated `From<corpus_engine_scip::Error>` impl (the
//! orphan rule forbids it in either crate); [`from_scip`] carries it instead,
//! and the three `?` sites name it explicitly.

pub use corpus_index::error::{Error, Result}; // shim: moved by domains REVIEW-build-index-read-port

/// Bridge the narrow `corpus-engine-scip::Error` into the engine's [`Error`].
///
/// Scip only constructs `Io` and `Database` variants, so the mapping is total.
/// This used to be a `From` impl on `Error`; moving `Error` to the leaf made
/// that impl an orphan, so it is a named function the three scip `?` sites
/// call. The mapping is unchanged (ARCH §18.3: a substitution you cannot name
/// is one you must not make).
#[cfg(feature = "treesitter")]
pub fn from_scip(e: corpus_engine_scip::Error) -> Error {
    match e {
        corpus_engine_scip::Error::Io(io) => Error::Io(io),
        corpus_engine_scip::Error::Database(s) => Error::Database(s),
        // No dedicated corpus-engine variant; fold into Database but keep
        // the "graph preserved" meaning in the message so callers/logs
        // still see WHY the export refused to complete.
        corpus_engine_scip::Error::ExportAborted(s) => {
            Error::Database(format!("export aborted (existing graph preserved): {s}"))
        }
    }
}
