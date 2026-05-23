//! Local error type for corpus-engine-notes.
//!
//! Same shape as the atos carve-out: `Io`, `Database`, `InvalidInput`.
//! `InvalidInput` covers the validation paths in NoteStore methods
//! that reject empty/malformed inputs before touching the database.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

pub type Result<T> = std::result::Result<T, Error>;
