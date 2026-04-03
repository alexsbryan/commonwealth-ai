/// Errors produced by Commonwealth library crates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("invalid join key: {0}")]
    InvalidJoinKey(String),

    #[error("membership error: {0}")]
    Membership(String),

    #[error("discovery error: {0}")]
    Discovery(String),

    #[error("gossip error: {0}")]
    Gossip(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("orchestrator error: {0}")]
    Orchestrator(String),

    #[error("{0}")]
    Internal(String),
}

/// Convenience alias used throughout library crates.
pub type Result<T> = std::result::Result<T, Error>;
