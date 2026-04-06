use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Recipe error: {0}")]
    Recipe(String),

    #[error("Extraction error: {0}")]
    Extraction(String),

    #[error("Embedding error: {0}")]
    Embed(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Index not found: {0}")]
    IndexNotFound(String),

    #[error("No shards found for corpus: {0}")]
    NoShardsFound(String),

    #[error("Already installed: {0}")]
    AlreadyInstalled(String),

    #[error("Incompatible embedding model: index uses '{index_model}', expected '{expected_model}' (path: {path})")]
    IncompatibleEmbedding {
        index_model: String,
        expected_model: String,
        path: PathBuf,
    },

    #[error("Safety violation: {0}")]
    Safety(String),
}
