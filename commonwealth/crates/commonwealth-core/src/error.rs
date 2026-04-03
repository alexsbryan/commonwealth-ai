/// Errors produced by Commonwealth library crates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("{0}")]
    Internal(String),
}

/// Convenience alias used throughout library crates.
pub type Result<T> = std::result::Result<T, Error>;
