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
        total: u64,
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

/// Thread-safe progress callback.
pub type ProgressCallback = Box<dyn Fn(IngestProgress) + Send>;
