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
