/// Progress updates during corpus ingestion.
#[derive(Debug, Clone)]
pub enum IngestProgress {
    Downloading {
        percent: f32,
        bytes_downloaded: u64,
        bytes_total: Option<u64>,
    },
    Extracting {
        documents_processed: u64,
    },
    Chunking {
        chunks_created: u64,
    },
    Embedding {
        chunks_embedded: u64,
        /// Total chunks expected. Zero means unknown (e.g. streaming extraction).
        total: u64,
        /// Number of source documents processed so far.
        docs_processed: u64,
        /// Embedding throughput in chunks per second over the last batch.
        chunks_per_sec: f32,
    },
    Indexing {
        chunks_indexed: u64,
        total: u64,
    },
    /// Enrichment phase 1: extracting claims from chunks.
    ExtractingClaims {
        current: u64,
        total: u64,
        /// Cumulative number of claims successfully extracted so far.
        claims_found: u64,
        /// Chunks where the inference call failed entirely.
        inference_errors: u64,
        /// Chunks where inference succeeded but JSON parsing produced nothing.
        parse_errors: u64,
        /// Throughput over the last reporting window (chunks per second).
        chunks_per_sec: f32,
    },
    /// Enrichment phase 2 (preliminary): candidate pairs identified.
    FoundCandidatePairs {
        count: usize,
    },
    /// Enrichment phase 2: extracting relationships between claim pairs.
    ExtractingRelationships {
        current: u64,
        total: u64,
    },
    /// Structural enrichment (Wikipedia): building link-based relationship graph.
    BuildingLinkGraph {
        current: usize,
        total: usize,
    },
    /// Structural enrichment (Wikipedia): computing per-article epistemic profiles.
    ComputingArticleProfiles {
        article_count: usize,
    },
    Complete {
        total_chunks: u64,
        duration_secs: u64,
    },
}

/// Thread-safe progress callback. Must be `Sync` because the engine's
/// async pipeline holds an `&Option<ProgressCallback>` across `.await`
/// points, which requires the callback itself to be safe to share by
/// reference between tasks.
pub type ProgressCallback = Box<dyn Fn(IngestProgress) + Send + Sync>;
