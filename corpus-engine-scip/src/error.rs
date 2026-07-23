// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local error type for corpus-engine-scip.
//!
//! Deliberately narrow: only the two variants scip actually constructs
//! (`Io` and `Database`). Pre-carve-out, scip used the full
//! `corpus_engine::Error` enum (15+ variants for safety, embedding,
//! recipes, etc.) even though it only needed two. Carrying the full
//! enum coupled every scip consumer to every corpus-engine concern.
//!
//! Callers in `corpus-engine` itself convert via the
//! `From<corpus_engine_scip::Error>` impl in `corpus_engine::error`.
//! Callers in the sovereign workspace generally `map_err` to their
//! own error type — see e.g. `sovereign-mesh/src/reindexer.rs` where
//! scip errors get stringified into the reindexer's local error
//! variant, and `sovereign-tools/src/code/*.rs` where they get
//! wrapped in `sovereign_core::Error::Tool`.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(String),

    /// A full SCIP export was refused because completing it would have
    /// destroyed the existing graph — e.g. an exporter that is present in
    /// PATH but fails at runtime (a broken rust-analyzer shim) yields zero
    /// symbols, and clearing/replacing on that would wipe a good index.
    /// `export_all` returns this INSTEAD of `Ok` so every caller (the CLI
    /// refresh and the daemon Reindexer alike) fails closed: the graph is
    /// left untouched and the operation reports failure rather than a
    /// silent "done" over an emptied index. Carries a human-readable reason.
    #[error("export aborted (existing graph preserved): {0}")]
    ExportAborted(String),
}

pub type Result<T> = std::result::Result<T, Error>;
