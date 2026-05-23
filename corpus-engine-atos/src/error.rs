//! Local error type for corpus-engine-atos.
//!
//! Narrow by design (`Io` + `Database`), mirroring the scip carve-out.
//! `From<corpus_engine_atos::Error> for corpus_engine::Error` lives in
//! `corpus-engine/src/error.rs` for `?`-bubbling inside corpus-engine.
//! External consumers either implement their own `From` or `.map_err`
//! at the call site — sovereign-atos already has the pattern.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(String),

    /// FeatureStore invalid-input validations (empty IDs, malformed
    /// milestone shapes, etc.). Kept here rather than mapped through
    /// `Database` because the callers want to distinguish "user gave
    /// us garbage" from "the SQLite layer hiccupped."
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

pub type Result<T> = std::result::Result<T, Error>;
