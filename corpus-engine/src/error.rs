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

    /// Cross-encoder rerank failure. Surfaced from
    /// `RerankFn` calls in `CorpusIndex::search_with_rerank`. The
    /// search path catches it, logs a warning, and falls back to the
    /// un-reranked fusion result — enabling rerank is purely
    /// additive, so a transient model issue must never degrade
    /// retrieval below baseline.
    #[error("Rerank error: {0}")]
    Rerank(String),

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

    #[error("Unknown enrichment domain: {0}")]
    UnknownEnrichmentDomain(String),

    #[error("Shard mismatch: {0}")]
    ShardMismatch(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// The ingest task observed a cancellation signal via
    /// [`CancellationFlag`](crate::CancellationFlag) and returned
    /// without completing. Not a failure per se — the caller (Desktop
    /// "Cancel" / `POST /internal/corpus/cancel`) asked for this.
    /// Distinct variant so callers can suppress the error log and run
    /// the wipe-everything cleanup path.
    #[error("Ingest of '{0}' cancelled by user")]
    Cancelled(String),
}
