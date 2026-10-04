// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local error type for corpus-engine-watchers.
//!
//! Deliberately narrow: the watcher + result-store code constructs
//! exactly one variant, `Io`. rusqlite failures are folded into `Io`
//! via the `sqlite_err` helpers (`std::io::Error::other`), and the
//! `notify` init/watch failures and the empty-`watch_paths` guard
//! likewise surface as `Io`. There are no corpus-engine-internal
//! consumers of these errors (the watchers are a leaf of the graph),
//! so — unlike the scip carve-out — no `From<Error> for
//! corpus_engine::Error` bridge is required.
//!
//! Add a variant here the first time a watcher path needs to fail for
//! a reason that is genuinely not I/O (per DECOMPOSITION.md pattern
//! lesson #5, `InvalidInput` is the usual next one).

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
