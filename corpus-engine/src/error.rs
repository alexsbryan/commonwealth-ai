// SPDX-License-Identifier: AGPL-3.0-or-later
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

    /// A download was refused with 401/403: the resource exists but this
    /// node is not authorised to fetch it.
    ///
    /// The canonical case is a HuggingFace dataset marked `gated:
    /// manual` (or `private: true`). Nothing upstream of the file fetch
    /// notices, because the repo *metadata* stays publicly readable —
    /// `GET /api/datasets/<repo>` returns 200 while
    /// `resolve/main/<file>` returns 401 `GatedRepo`. So the failure
    /// lands mid-install rather than at catalog time.
    ///
    /// Its own variant rather than a folded-in [`Error::Http`] because
    /// the remedy is a specific human action against a specific URL, and
    /// because this message is shown to the user verbatim via
    /// [`crate::IngestProgress::Failed`]. A bare "HTTP status 401" is
    /// not actionable; "request access at <url>, then set HF_TOKEN" is.
    #[error("{remedy}")]
    DownloadUnauthorized {
        url: String,
        status: u16,
        remedy: String,
    },

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

    /// A prebuilt snapshot's embedding space is incompatible with the
    /// locally-loaded model — different vector dimensions, or a model-name
    /// mismatch whose re-embedding probe fell below the similarity
    /// threshold (the spaces genuinely differ). Distinct variant so
    /// `ingest()` can fall through to a full ingest (rebuild with the
    /// local model) instead of hard-failing.
    #[error("Snapshot incompatible with local embedding model: {0}")]
    SnapshotIncompatible(String),

    #[error("Safety violation: {0}")]
    Safety(String),

    #[error("Unknown enrichment domain: {0}")]
    UnknownEnrichmentDomain(String),

    /// A recipe's `[enrichment] type` names no registered pass. Refused at
    /// recipe load by `recipe_parsing::check_enrichment_type`; the valid set
    /// comes from `EnrichmentPassRegistry::builtin` so the message never
    /// goes stale against the registry (§10.6, §18.3).
    #[error("Unknown enrichment type \"{got}\" — valid [enrichment] type values are: {valid}")]
    UnknownEnrichmentType { got: String, valid: String },

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

impl Error {
    /// Build a [`Error::DownloadUnauthorized`] whose message tells the
    /// operator what to actually do about a 401/403.
    ///
    /// The remedy is derived from the URL shape so the knowledge lives
    /// in exactly one place (and is unit-testable without a network):
    /// a HuggingFace `resolve` URL yields the repo's landing page —
    /// which is where the "Request access" button lives for a gated
    /// dataset — plus the `HF_TOKEN` hint, since access alone is not
    /// enough if the download is unauthenticated. Anything else gets a
    /// generic but still honest message naming the status and URL.
    pub fn download_unauthorized(url: &str, status: u16) -> Self {
        let remedy = match hf_repo_page(url) {
            Some(page) => format!(
                "Download refused ({status}) — {page} is private or gated, \
                 so the file cannot be fetched anonymously. Request access at \
                 {page}, then set HF_TOKEN in the environment before retrying. \
                 (The repo's metadata is public even when the files are not, \
                 which is why this surfaced only now, mid-install.)"
            ),
            None => format!(
                "Download refused ({status}) for {url} — the server rejected \
                 this request as unauthorised. If the resource needs \
                 credentials, supply them and retry."
            ),
        };
        Error::DownloadUnauthorized {
            url: url.to_string(),
            status,
            remedy,
        }
    }
}

/// Map a HuggingFace `resolve`-style download URL back to the repo's
/// human landing page, which is where access is requested for a gated
/// repo. Returns `None` for any URL that isn't recognisably HuggingFace,
/// so callers fall back to a generic message rather than inventing a
/// link that doesn't exist.
///
/// Handles both namespaces:
///   `https://huggingface.co/datasets/<owner>/<repo>/resolve/main/<file>`
///     → `https://huggingface.co/datasets/<owner>/<repo>`
///   `https://huggingface.co/<owner>/<repo>/resolve/main/<file>`
///     → `https://huggingface.co/<owner>/<repo>`
fn hf_repo_page(url: &str) -> Option<String> {
    const HOST: &str = "https://huggingface.co/";
    let rest = url.strip_prefix(HOST)?;
    // Everything before `/resolve/` is the repo path. Without a
    // `/resolve/` segment we can't reliably tell repo from file, so
    // decline rather than guess.
    let repo_path = rest.split("/resolve/").next()?;
    if repo_path.is_empty() || repo_path == rest {
        return None;
    }
    // `datasets/<owner>/<repo>` (3 segments) or `<owner>/<repo>` (2).
    let segments = repo_path.split('/').count();
    if segments < 2 {
        return None;
    }
    Some(format!("{HOST}{repo_path}"))
}

/// Bridge `oplog::OplogError` into our wider Error.
///
/// The mapping is chosen to be OBSERVATIONALLY EMPTY: before the journal moved
/// out of this crate (2026-09-04) its call sites raised `Error::Io` and
/// `Error::Extraction("{label}: serialise: {e}")` directly, and they still do,
/// with byte-identical `Display` text. That is the whole point of the facade —
/// a consumer cannot tell the extraction happened, so none of the ~20 of them
/// had to change (ARCH §18.3: a substitution you cannot name is one you must
/// not make).
impl From<oplog::OplogError> for Error {
    fn from(e: oplog::OplogError) -> Self {
        match e {
            oplog::OplogError::Io(io) => Error::Io(io),
            other @ oplog::OplogError::Serialise { .. } => Error::Extraction(other.to_string()),
        }
    }
}

/// Bridge the narrow `corpus-engine-scip::Error` into our wider Error.
/// Scip only constructs `Io` and `Database` variants, so the mapping
/// is total. Required for `?`-bubbling from scip-returning helpers
/// inside `update::watch::CodeWatcher` and
/// `enrichment::atlas::strategies::code_walk`, which return our local
/// `Result<T>`. External sovereign-* consumers don't go through this
/// — they generally `map_err` into their own error type.
#[cfg(feature = "treesitter")]
impl From<corpus_engine_scip::Error> for Error {
    fn from(e: corpus_engine_scip::Error) -> Self {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SEP case, which is what motivated the variant: a gated
    /// dataset's file URL must yield the dataset's landing page, because
    /// that page is where "Request access" lives.
    #[test]
    fn hf_dataset_url_yields_repo_landing_page() {
        let page = hf_repo_page(
            "https://huggingface.co/datasets/svrnmesh/sep-index/resolve/main/sep-qwen-embedding-0.6b-2026-06-29.tar.zst",
        );
        assert_eq!(
            page.as_deref(),
            Some("https://huggingface.co/datasets/svrnmesh/sep-index")
        );
    }

    /// Model repos use the same `resolve` shape without the `datasets/`
    /// segment — the GGUF download path.
    #[test]
    fn hf_model_url_yields_repo_landing_page() {
        let page =
            hf_repo_page("https://huggingface.co/svrnmesh/some-model/resolve/main/weights.gguf");
        assert_eq!(
            page.as_deref(),
            Some("https://huggingface.co/svrnmesh/some-model")
        );
    }

    /// Anything we can't confidently parse must decline rather than
    /// fabricate a link — a wrong "request access here" is worse than a
    /// generic message.
    #[test]
    fn non_huggingface_and_malformed_urls_decline() {
        assert_eq!(hf_repo_page("https://example.com/data/file.tar.zst"), None);
        // No `/resolve/` segment: repo vs file is unknowable.
        assert_eq!(
            hf_repo_page("https://huggingface.co/datasets/svrnmesh/sep-index"),
            None
        );
        // One segment before `/resolve/` is not an `<owner>/<repo>`
        // pair, so there is no landing page to point at.
        assert_eq!(
            hf_repo_page("https://huggingface.co/lonely/resolve/main/x.bin"),
            None
        );
        assert_eq!(
            hf_repo_page("https://huggingface.co/resolve/main/x.bin"),
            None
        );
    }

    /// The whole point of the variant is that its `Display` output is
    /// actionable. Assert on the substance a user needs — the repo to
    /// request access from, and the env var to set — not on prose.
    #[test]
    fn gated_download_error_names_the_repo_and_the_token() {
        let err = Error::download_unauthorized(
            "https://huggingface.co/datasets/svrnmesh/sep-index/resolve/main/snap.tar.zst",
            401,
        );
        let msg = err.to_string();
        assert!(
            msg.contains("https://huggingface.co/datasets/svrnmesh/sep-index"),
            "must name the repo page where access is requested: {msg}"
        );
        assert!(
            msg.contains("HF_TOKEN"),
            "must name the token env var: {msg}"
        );
        assert!(msg.contains("401"), "must name the status: {msg}");
    }

    /// A non-HuggingFace 403 still has to say something true and useful
    /// rather than pointing at an invented HuggingFace page.
    #[test]
    fn generic_unauthorized_error_does_not_invent_a_huggingface_link() {
        let err = Error::download_unauthorized("https://example.com/private.tar.zst", 403);
        let msg = err.to_string();
        assert!(msg.contains("403"), "{msg}");
        assert!(msg.contains("https://example.com/private.tar.zst"), "{msg}");
        assert!(
            !msg.contains("HF_TOKEN") && !msg.contains("huggingface"),
            "must not suggest HuggingFace remedies for a non-HF host: {msg}"
        );
    }
}
