use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("store backend error: {0}")]
    Backend(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("node id error: {0}")]
    NodeId(String),
}

pub type Result<T> = std::result::Result<T, Error>;
