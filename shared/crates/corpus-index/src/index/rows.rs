// SPDX-License-Identifier: AGPL-3.0-or-later
//! The row types the index writes and reads: what an insert carries, and the
//! projections the read paths return. Moved out of `index/mod.rs` unchanged.

/// Typed code-intelligence metadata for a single chunk. Populated by the
/// `code` extractor and promoted into the typed schema columns by the
/// insert path; `None` for non-code corpora.
#[derive(Clone, Debug, Default)]
pub struct InsertCodeMeta {
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub file_path: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub language: Option<String>,
    pub mtime: Option<i64>,
}

/// Extract code-intelligence fields from an `ExtractedDoc.metadata` JSON
/// object. Returns an empty `InsertCodeMeta` if the metadata is missing or
/// doesn't carry code-specific keys — in that case every code column is
/// stored as Null and the chunk behaves like any other non-code chunk.
pub fn code_meta_from_json(metadata: Option<&serde_json::Value>) -> InsertCodeMeta {
    let Some(obj) = metadata.and_then(|v| v.as_object()) else {
        return InsertCodeMeta::default();
    };
    InsertCodeMeta {
        symbol_name: obj
            .get("symbol_name")
            .and_then(|v| v.as_str())
            .map(String::from),
        symbol_kind: obj
            .get("symbol_kind")
            .and_then(|v| v.as_str())
            .map(String::from),
        file_path: obj
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from),
        line_start: obj
            .get("line_start")
            .and_then(|v| v.as_i64())
            .map(|n| n as i32),
        line_end: obj
            .get("line_end")
            .and_then(|v| v.as_i64())
            .map(|n| n as i32),
        language: obj
            .get("language")
            .and_then(|v| v.as_str())
            .map(String::from),
        mtime: obj.get("mtime").and_then(|v| v.as_i64()),
    }
}

/// A chunk to be inserted into the index.
#[derive(Clone)]
pub struct InsertChunk {
    pub content: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub metadata: Option<String>, // JSON string
    /// BLAKE3 hex digest of the chunk text, populated during ingestion.
    pub content_hash: Option<String>,
    /// Document-level grouping key (article URL, DOI, etc.).
    /// Used by delta updates to delete/replace all chunks from one document.
    pub source_doc_id: Option<String>,
    /// Source file this chunk came from (filename only, e.g.
    /// `"train-00021-of-00041.parquet"`). Populated by multi-shard
    /// extractors. Used to track per-file commit progress and drive
    /// collaborative ingestion partition boundaries.  `None` for
    /// single-file and code corpora.
    pub source_file: Option<String>,
    /// Optional code-intelligence metadata. `Default::default()` means
    /// non-code chunk — all code columns will be Null.
    pub code: InsertCodeMeta,
    /// Optional pull-based work queue unit id. Stamped onto every chunk
    /// produced by a leased unit so that if lease expiry causes two peers
    /// to process the same unit, the merge leader can dedupe by
    /// `(unit_id, peer_id)` groups and keep only the earliest-completed
    /// peer's chunks. `None` for legacy (static-partition) ingest and
    /// local Desktop-driven ingest, which have no shared work queue.
    pub unit_id: Option<u32>,
    /// The stored text this chunk was cut from, by its published name
    /// (`<corpus>/texts/<sha256>`). `None` when no text was stored: a
    /// producer with no document, or a corpus that predates the text store.
    pub text_sha256: Option<kernel_types::Sha256Hash>,
}

/// A pre-embedded chunk ready for direct insertion.
pub struct EmbeddedChunk {
    pub insert: InsertChunk,
    pub embedding: Vec<f32>,
}

/// A chunk read out of an index, used by the enrichment pipeline.
#[derive(Debug, Clone)]
pub struct StoredChunk {
    pub id: u64,
    pub content: String,
    pub title: Option<String>,
    /// Source-document id this chunk belongs to (the root-relative
    /// file path for local/watched corpora). The vault preview keys
    /// its note rollup on this — the humanised `title` is
    /// display-grade and not a valid file path.
    pub source_doc_id: Option<String>,
}

/// A chunk with its raw metadata JSON string, used by the structural
/// enrichment pipeline (link graph builder and article profile builder).
#[derive(Debug, Clone)]
pub struct StoredChunkWithMetadata {
    pub id: u64,
    pub title: Option<String>,
    pub url: Option<String>,
    pub metadata_raw: Option<String>,
}

/// Full-content chunk row used by adapters that need to reconstruct
/// the source text (atlas pipeline `--from-corpus` synthesises a
/// `ChapterManifest` from these). Carries every column the v2
/// enrichment pipeline's per-section extraction needs to operate
/// on an already-indexed multi-document corpus without re-chunking
/// from a single source file.
#[derive(Debug, Clone)]
pub struct EnrichmentChunkRow {
    pub id: u64,
    pub content: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub metadata_raw: Option<String>,
    pub source_doc_id: Option<String>,
}

/// Counts produced by
/// [`CorpusIndex::dedupe_chunk_rows`](super::CorpusIndex::dedupe_chunk_rows).
/// The caller logs/displays these so the operator can see how much of
/// their compute was duplicate work.
#[derive(Debug, Clone, Copy, Default)]
pub struct DedupeReport {
    /// Total rows in the table before the dedupe pass.
    pub rows_before: u64,
    /// Total rows in the table after the dedupe pass.
    pub rows_after: u64,
    /// Number of rows deleted because their [`ChunkKey`](super::ChunkKey) (document and
    /// text hash) repeated a row with a smaller `id`.
    pub duplicates_deleted: u64,
    /// Number of distinct chunk keys preserved (= number of "winning"
    /// rows kept). Plus any hashless rows, this is the post-dedupe row
    /// count.
    pub unique_keys_kept: u64,
    /// Rows where `content_hash` was null. Pre-existing legacy
    /// rows from before the field was populated. Left untouched
    /// because we have no signal to dedup them safely.
    pub hashless_rows_preserved: u64,
}

impl DedupeReport {
    /// Convenience: did the pass actually delete anything?
    pub fn changed(&self) -> bool {
        self.duplicates_deleted > 0
    }

    /// Duplication rate as a fraction in [0.0, 1.0). Returns 0.0
    /// when the table was empty or had no hashed rows.
    pub fn dup_fraction(&self) -> f64 {
        let hashed = self.unique_keys_kept + self.duplicates_deleted;
        if hashed == 0 {
            0.0
        } else {
            self.duplicates_deleted as f64 / hashed as f64
        }
    }
}
