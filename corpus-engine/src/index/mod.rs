// SPDX-License-Identifier: AGPL-3.0-or-later
//! CorpusIndex — wraps a LanceDB table for a per-corpus index
//! with IVF-PQ vector search and Tantivy full-text search.

mod create;
mod enrichment;
pub mod raptor;
mod read;
mod search;
mod write;

pub use read::NeighborWindow;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow::datatypes::{DataType, Field, Schema};
use arrow_schema::SchemaRef;

use crate::error::{Error, Result};
use crate::types::{ChunkRange, IndexInfo};

// ─── Helper types ──────────────────────────────────────────

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

/// Counts produced by [`CorpusIndex::dedupe_by_content_hash`]. The
/// caller logs/displays these so the operator can see how much of
/// their compute was duplicate work.
#[derive(Debug, Clone, Copy, Default)]
pub struct DedupeReport {
    /// Total rows in the table before the dedupe pass.
    pub rows_before: u64,
    /// Total rows in the table after the dedupe pass.
    pub rows_after: u64,
    /// Number of rows deleted because their `content_hash` was a
    /// duplicate of a row with a smaller `id`.
    pub duplicates_deleted: u64,
    /// Number of distinct content_hashes preserved (= number of
    /// "winning" rows kept). Plus any hashless rows, this is the
    /// post-dedupe row count.
    pub unique_hashes_kept: u64,
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
        let hashed = self.unique_hashes_kept + self.duplicates_deleted;
        if hashed == 0 {
            0.0
        } else {
            self.duplicates_deleted as f64 / hashed as f64
        }
    }
}

// ─── CorpusIndex ───────────────────────────────────────────

/// A single corpus index backed by LanceDB.
/// Uses IVF-PQ for vector search and Tantivy for full-text search.
///
/// The on-disk `_corpus_meta.json` is the single source of truth for index
/// metadata. We cache only the two fields read on every operation
/// (`corpus_id` for identity, `embedding_dimensions` for vector ops); the
/// rest is loaded on-demand via `info()`. This avoids stale-cache bugs when
/// callers like `set_shard_meta()` mutate the metadata file.
///
/// `Clone` is cheap: `lancedb::Connection`/`Table` are `Arc`-backed
/// handles, so a clone shares the same open dataset. This lets
/// `CorpusEngine` cache opened indexes and hand out clones (see
/// `open_index`) instead of re-opening LanceDB on every search.
#[derive(Clone)]
pub struct CorpusIndex {
    db: lancedb::Connection,
    table: lancedb::Table,
    corpus_id: String,
    embedding_dimensions: usize,
}

/// Build the Arrow schema for a corpus index table.
///
/// Columns fall into two groups:
/// - **Base** (`id`, `content`, `title`, `url`, `embedding`, `metadata`,
///   `content_hash`, `source_doc_id`) — populated by every corpus type.
/// - **Code intelligence** (`symbol_name`, `symbol_kind`, `file_path`,
///   `line_start`, `line_end`, `language`, `mtime`) — nullable. Populated
///   only by code corpora; Wikipedia/SEP etc. leave them Null. Real
///   columns (not JSON) so LanceDB filter pushdown keeps symbol-lookup
///   under 10ms.
pub(crate) fn corpus_schema(embedding_dim: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        // Base columns
        Field::new("id", DataType::Int64, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, true),
        Field::new("url", DataType::Utf8, true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                embedding_dim as i32,
            ),
            false,
        ),
        Field::new("metadata", DataType::Utf8, true),
        Field::new("content_hash", DataType::Utf8, true),
        Field::new("source_doc_id", DataType::Utf8, true),
        // Code-intelligence columns (nullable for non-code corpora)
        Field::new("symbol_name", DataType::Utf8, true),
        Field::new("symbol_kind", DataType::Utf8, true),
        Field::new("file_path", DataType::Utf8, true),
        Field::new("line_start", DataType::Int32, true),
        Field::new("line_end", DataType::Int32, true),
        Field::new("language", DataType::Utf8, true),
        Field::new("mtime", DataType::Int64, true),
        // Pull-based work queue column: the UnitId that produced this chunk.
        // Null for legacy ingests (static partitioning, local Desktop install).
        // Populated when ingest runs under a WorkQueueManager lease so the
        // merge step can dedupe re-processed units across peer partition dirs.
        Field::new("unit_id", DataType::Int32, true),
    ]))
}

/// Current on-disk schema version. Bumped when `corpus_schema()` changes
/// in a way that requires an LanceDB `add_columns` migration on open.
///
/// - v1: base + embedding only.
/// - v2: added code-intelligence columns (symbol_name, symbol_kind, file_path,
///   line_start, line_end, language, mtime).
/// - v3: added `unit_id` for pull-based queue dedup.
pub(crate) const CURRENT_INDEX_SCHEMA_VERSION: u32 = 3;

fn default_index_schema_version() -> u32 {
    1 // Files without the field predate versioning → treat as v1.
}

/// Metadata stored as a JSON file alongside the LanceDB table.
#[derive(serde::Serialize, serde::Deserialize)]
struct IndexMeta {
    corpus_id: String,
    corpus_name: String,
    embedding_model: String,
    embedding_dimensions: usize,
    mesh_sharing: bool,
    /// Whether peers on a shared Commonwealth mesh may run
    /// knowledge-search queries against this index. `None` means
    /// "no explicit value — pre-split index, fall back to
    /// `mesh_sharing`". Set by the recipe at ingest time.
    #[serde(default)]
    query_sharing: Option<bool>,
    /// Recipe `[retrieval] dedup_by_source`. `None` = legacy index
    /// predating the field → resolves to `false`. Stamped post-create by
    /// `set_dedup_by_source` (mirrors `set_display` / `set_mutable_merge`).
    #[serde(default)]
    dedup_by_source: Option<bool>,
    /// Recipe `[retrieval] personal_scope`. `None` = legacy index
    /// predating the field → resolves to `false`. Stamped post-create by
    /// `set_personal_scope` (mirrors `set_dedup_by_source`).
    #[serde(default)]
    personal_scope: Option<bool>,
    license: String,
    created_at: u64,
    last_updated: u64,
    /// Version of the LanceDB column layout used by this index. Opened
    /// indexes with a version lower than `CURRENT_INDEX_SCHEMA_VERSION`
    /// are migrated in place via `Table::add_columns`.
    #[serde(default = "default_index_schema_version")]
    schema_version: u32,
    #[serde(default)]
    is_shard: bool,
    #[serde(default)]
    chunk_range_start: Option<u64>,
    #[serde(default)]
    chunk_range_end: Option<u64>,
    /// Set to true when ingestion starts, cleared to false only on successful
    /// completion. If the process is killed mid-ingest this stays true, allowing
    /// `installed_indexes()` to skip the partial index rather than reporting it
    /// as fully installed. Defaults to false so existing complete indexes (written
    /// before this field existed) are treated as complete.
    #[serde(default)]
    ingestion_in_progress: bool,
    /// Number of source documents iterated (including skipped/errored) at the
    /// last successful batch flush. Used to resume a killed ingest from the
    /// correct position without re-embedding already-committed chunks.
    /// Defaults to 0 so existing complete indexes don't try to resume.
    #[serde(default)]
    committed_iter_pos: u64,

    /// Snapshot of the assigned-shard set at the time `committed_iter_pos`
    /// was last written. The iter_pos counter is meaningful only within
    /// the iteration produced by THIS shard set — if a subsequent run
    /// resolves a different set (because `processed_shards` mutated
    /// between runs), the same `committed_iter_pos` value lands at a
    /// different point in the source and either skips real work or
    /// short-circuits the loop entirely. The resume logic compares the
    /// saved set to the current resolved set and falls back to a
    /// document-level skipset when they diverge.
    ///
    /// `None` for legacy indexes written before this field existed —
    /// resume in that case is best-effort: we treat the iter_pos as
    /// authoritative and log a warning if the current run produces
    /// zero embeddings (likely the bug surfaced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    committed_shard_set: Option<Vec<usize>>,
    /// Set to true once `build_indexes()` completes successfully.
    /// Allows a resume to skip index-building if it already finished in a
    /// previous run (e.g. process killed between build_indexes and
    /// mark_ingestion_complete). Defaults to false.
    #[serde(default)]
    indexes_built: bool,
    /// Per-sub-phase checkpoints within build_indexes(). Allow a resume to skip
    /// whichever sub-indexes were already built before a kill/crash, avoiding
    /// multi-hour rebuilds of completed work.
    #[serde(default)]
    vector_index_built: bool,
    #[serde(default)]
    content_fts_built: bool,
    #[serde(default)]
    title_fts_built: bool,

    /// Set once `dedupe_by_content_hash()` has run successfully on
    /// this index. Used by `build_indexes()` to skip re-running the
    /// pre-build dedupe pass on a resume — it's idempotent (a clean
    /// index dedups to a no-op) but the table scan is wasted I/O on
    /// every resume. Indexes that pre-date this field default to
    /// `false` and will be deduped exactly once on their next build.
    #[serde(default)]
    chunks_deduped: bool,

    /// High-water mark for chunk-id allocation: the id the NEXT inserted
    /// chunk will receive. Bumped by the batch size on every insert and
    /// NEVER decreases, so ids are globally unique and monotonic even
    /// across deletes, dedupes, and incremental (delta) appends.
    ///
    /// This is the AUTHORITATIVE id source. The previous scheme derived
    /// ids from `chunk_count()` (the row count), which silently diverges
    /// from the max id after any delete/dedupe/delta — the next insert
    /// then REUSED ids that already existed, making `neighbors(id)`
    /// ambiguous and citations resolve to the wrong chunk (the
    /// duplicate-id corruption: 31k colliding rows in the wikipedia
    /// index, "2026 Lebanon war" reading back as "Gold").
    ///
    /// `None` for legacy indexes written before this field existed →
    /// seeded lazily from `max(id) + 1` on the first insert.
    #[serde(default)]
    next_chunk_id: Option<u64>,

    // ── Health-check fields ──────────────────────────────────
    /// Expected total chunks, written at ingest start.
    #[serde(default)]
    chunks_expected: Option<u64>,
    /// Resume cursor (batch ID) from last checkpoint. Same as
    /// committed_iter_pos semantically but expressed as a string batch key
    /// for compatibility with the CorpusUpdater progress log.
    #[serde(default)]
    resume_from: Option<String>,
    /// True if the enrichment pipeline has been run at least once.
    #[serde(default)]
    enrichment_enabled: bool,
    /// Count of chunks that have at least one extracted claim.
    #[serde(default)]
    enriched_chunks: Option<u64>,
    /// Source version token (date stamp or manifest hash).
    #[serde(default)]
    source_version: Option<String>,
    /// Manifest URL for update checks.
    #[serde(default)]
    update_manifest_url: Option<String>,

    /// Absolute source directory for code corpora, captured at ingest
    /// time from the recipe's `[acquire] local_file path`. Used by the
    /// `sovereign code watch` subcommand so the watcher knows where to
    /// observe. `None` for non-code corpora; present with `#[serde(default)]`
    /// so pre-v2 indexes deserialize cleanly.
    #[serde(default)]
    source_path: Option<String>,

    /// ZIP shard indices that have been fully committed for this
    /// corpus. Populated by the JSONL extractor at shard boundaries
    /// and used by the collaborative-ingestion coordinator to decide
    /// which shards are still outstanding when computing a sharded
    /// partition plan. Empty for corpora that do not ingest from a
    /// multi-shard ZIP.
    #[serde(default)]
    processed_shards: Vec<usize>,

    /// Total number of source shards the extractor expects for this
    /// corpus. Stamped at ingest start by the sharded extractor (e.g.
    /// the Wikipedia JSONL extractor counts ZIP entries) so a later
    /// `corpus diag` can compute "missing shards" correctly even
    /// when the trailing shards never started — the
    /// `processed_shards` list alone undercounts because it can't
    /// see beyond max(processed). `None` for non-sharded corpora and
    /// for legacy indexes that pre-date this field.
    #[serde(default)]
    total_shards: Option<usize>,

    /// Active filter scope, if any. Records what subset of the source
    /// the index covers — e.g. Wikipedia Core's "top 100K by pageview
    /// rank ∪ Vital Articles". Drives the Settings "Expand" affordance
    /// (`expandable=true`) and the delta-update path (a different
    /// `filter_signature` on a re-ingest means a new scope and triggers
    /// a delta).
    ///
    /// Absent on legacy indexes that pre-date filter support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<ScopeMeta>,

    /// How this index came to exist on this node. `SelfInitiated`
    /// means a user click or CLI on THIS machine started it
    /// (canonical install or solo run). `PeerPulled` means a
    /// coordinator on another machine handed work to us via the
    /// pull-loop / corpus_collaborate path; the partition lives only
    /// for the duration of the pull and merges back to the
    /// coordinator's canonical when done.
    ///
    /// Used by `auto_resume::spawn_resume_in_progress_ingests` to
    /// decide whether a daemon restart should re-fire the ingest:
    /// SelfInitiated → yes (the user wants their install to keep
    /// going), PeerPulled → no (the coordinator will re-issue the
    /// handoff if it still wants the work; locally re-firing
    /// competes with foreground inference and undoes the user's
    /// `pause` from another machine).
    ///
    /// `#[serde(default)]` → SelfInitiated for any pre-provenance
    /// meta on disk, preserving today's behaviour for existing
    /// installs.
    #[serde(default)]
    provenance: CorpusProvenance,

    /// Filter pipeline override applied to this corpus, if any. Set by
    /// `expand_corpus` so a restart mid-expansion resumes with the
    /// correct scope (rather than the original recipe's narrower
    /// filter). When `Some`, it shadows `recipe.filters` for this
    /// corpus until the expansion completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filter_override: Option<FilterOverride>,

    /// Explicit kind. When `None`, `info()` derives kind from
    /// `source_path` (Some → Code, None → Knowledge) for back-compat
    /// with indexes written before this field existed. New ingests
    /// always stamp the explicit kind from `Recipe::corpus.kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<crate::types::CorpusKind>,

    /// Parent corpus id for per-work corpora produced by an on-demand
    /// catalog ingest. See `IndexInfo::parent_corpus_id`. `None` for
    /// stand-alone corpora.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_corpus_id: Option<String>,

    /// Stable identity for the *content* of this canonical index.
    /// Computed as `blake3(sorted(content_hash))` over every chunk
    /// row's `content_hash` column at canonical-write time. Two
    /// nodes that ran the same ingest with the same source data
    /// arrive at byte-identical fingerprints; mismatched
    /// fingerprints with overlapping `chunk_count` mean one node
    /// has more (or different) content than the other.
    ///
    /// Drives the mesh's canonical-sync path: a peer's gossiped
    /// `canonical_fingerprint` is the receipt the puller validates
    /// against after fetching the tarball, so a poisoned transfer
    /// fails closed.
    ///
    /// `None` for legacy canonicals written before this field
    /// existed and for partition indexes (which carry only their
    /// shard's worth of chunks; the fingerprint over a partition
    /// would be meaningless to compare against another peer's
    /// canonical). The fingerprint helper stamps the field on
    /// next-read for legacy canonicals so the upgrade is silent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_fingerprint: Option<String>,

    /// Reconciliation policy for `merge_shards`. Mirrors the recipe's
    /// `[corpus] mutable_merge` field; `None` means classic
    /// content-hash dedupe. Stamped at ingest time and propagated
    /// from the first input shard to a merged canonical so the
    /// policy survives shard→canonical→shard round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mutable_merge: Option<crate::recipe::MutableMergePolicy>,

    /// Stream-axis block (Move 5, Stage 2). Per-corpus stability tag
    /// + provenance summary. `None` for legacy indexes written before
    /// the stream taxonomy landed; backfilled lazily by
    /// `sovereign corpus stream-axes`. Articulation lives per-atom on
    /// meta-atlas anchors, not here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream: Option<crate::stream_axes::StreamAxes>,

    /// Presentation hints from the recipe's `[display]` block —
    /// `category` + `icon`. Pure UI metadata; the retrieval layer
    /// reads `category` when rendering "From your conversations"
    /// labels and the Atlas View rail groups by it.
    /// `None` on legacy indexes ingested before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display: Option<crate::recipe::DisplayMeta>,

    /// Move 6 P5.b/c: opt-in flag for the post-update incremental
    /// atlas hook. When `Some(true)` and the atlas at
    /// `<index_dir>/atlas/` carries content-hash IDs, the
    /// `CorpusUpdater::apply_update` post-phase hook fires
    /// `apply_atom_delta` with per-doc removals + (for structural
    /// corpora) per-doc re-extractions. Defaults to `None` —
    /// existing watched-folder / delta-ingest pipelines keep their
    /// pre-Move-6 behaviour until the user opts the corpus in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    atlas_incremental_enabled: Option<bool>,
}

/// Provenance of an on-disk index. See the `IndexMeta::provenance`
/// field for full semantics; in short, `SelfInitiated` means "this
/// node started the install" and `PeerPulled` means "a coordinator
/// on another node handed us a partition." Auto-resume only re-fires
/// SelfInitiated entries after a daemon restart.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CorpusProvenance {
    /// User-driven install on this machine. Default for any meta
    /// file written before this field existed (preserves the
    /// legacy auto-resume contract for existing installs).
    #[default]
    SelfInitiated,
    /// Partition assigned by a coordinator on another node via the
    /// collaborate-pull path. Lives only for the duration of the
    /// pull; merges to the coordinator's canonical on completion.
    PeerPulled,
}

/// Read just the `provenance` field from an on-disk meta file.
/// Returns the default (`SelfInitiated`) when the file is missing
/// or malformed — auto-resume's contract is "if in doubt, resume."
pub fn read_provenance(index_dir: &Path) -> CorpusProvenance {
    let path = meta_path(index_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return CorpusProvenance::default();
    };
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default)]
        provenance: CorpusProvenance,
    }
    serde_json::from_str::<Probe>(&raw)
        .map(|p| p.provenance)
        .unwrap_or_default()
}

/// Stamp the `provenance` field onto an existing meta file. Used by
/// pull-loops to mark a partition `PeerPulled` right after the
/// partition's `_corpus_meta.json` is first created. Idempotent —
/// repeated calls with the same value are no-ops on disk semantics.
/// Errors when the meta file is missing or malformed.
/// Stamp the stream-axis block onto an index by `index_dir` path,
/// without needing a [`CorpusIndex`] handle (no LanceDB open). Used
/// by `sovereign corpus stream-axes` to backfill the block for
/// installed corpora that lack it.
///
/// Reads `<index_dir>/_corpus_meta.json`, sets the `stream` field,
/// rewrites. Errors if the meta is missing.
pub fn set_stream_axes(index_dir: &Path, axes: crate::stream_axes::StreamAxes) -> Result<()> {
    let mut meta = read_meta(index_dir)?;
    meta.stream = Some(axes);
    write_meta(index_dir, &meta)
}

pub fn set_provenance(index_dir: &Path, provenance: CorpusProvenance) -> Result<()> {
    let meta = read_meta(index_dir)?;
    let updated = IndexMeta { provenance, ..meta };
    write_meta(index_dir, &updated)
}

/// Snapshot of the filter pipeline that's currently in force.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct ScopeMeta {
    /// Human-readable list of active filters, in order, e.g.
    /// `["pageview rank ≤ 100000 (98421 titles)", "title in `vital_articles_l5` (49 titles)"]`.
    pub filter_descriptions: Vec<String>,
    /// Stable hash of the canonical filter config (sha256 hex). Empty
    /// when no filter is active.
    pub filter_signature: String,
    /// `true` when the filter is non-empty and could be relaxed (i.e.
    /// the corpus could be expanded). `false` once the filter is
    /// removed (full corpus indexed) or the corpus had no filter to
    /// begin with.
    pub expandable: bool,
}

/// Persisted filter override (mirrors `Recipe.filters` +
/// `Recipe.filter_mode.mode` for the run currently in progress).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct FilterOverride {
    pub filters: Vec<crate::filters::FilterConfig>,
    pub mode: crate::filters::ComposeMode,
}

fn meta_path(index_dir: &Path) -> std::path::PathBuf {
    index_dir.join("_corpus_meta.json")
}

/// Migrate an on-disk index from `from_version` to the current schema
/// version. Additive-only (new nullable columns); existing data is
/// untouched. Safe to call on a partially-migrated index — LanceDB's
/// `add_columns` is a no-op for columns that already exist, so retries
/// after a crashed migration converge on the correct schema.
///
/// - v1 → v2 adds the seven code-intelligence columns.
/// - v2 → v3 adds `unit_id` for pull-based queue dedup.
async fn migrate_schema(table: &lancedb::Table, from_version: u32) -> Result<()> {
    if from_version >= CURRENT_INDEX_SCHEMA_VERSION {
        return Ok(());
    }

    use lancedb::table::NewColumnTransform;

    // Check which columns are actually missing — a previous migration
    // attempt may have added some and then crashed. We build the Arrow
    // schema for exactly the columns that don't exist yet, so retries
    // are idempotent.
    let current = table
        .schema()
        .await
        .map_err(|e| Error::Database(format!("read schema: {e}")))?;
    let existing: std::collections::HashSet<&str> =
        current.fields().iter().map(|f| f.name().as_str()).collect();

    // Union of all columns introduced after v1. The filter below keeps
    // only those not already present on disk, so an index already at v2
    // only gets `unit_id`, and an index at v3 gets nothing.
    let wanted: &[(&str, DataType)] = &[
        ("symbol_name", DataType::Utf8),
        ("symbol_kind", DataType::Utf8),
        ("file_path", DataType::Utf8),
        ("line_start", DataType::Int32),
        ("line_end", DataType::Int32),
        ("language", DataType::Utf8),
        ("mtime", DataType::Int64),
        ("unit_id", DataType::Int32),
    ];

    let missing: Vec<Field> = wanted
        .iter()
        .filter(|(name, _)| !existing.contains(name))
        .map(|(name, dtype)| Field::new(*name, dtype.clone(), true))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    let add_schema = Arc::new(Schema::new(missing));
    table
        .add_columns(NewColumnTransform::AllNulls(add_schema), None)
        .await
        .map_err(|e| Error::Database(format!("add_columns migration: {e}")))?;

    tracing::info!(
        from = from_version,
        to = CURRENT_INDEX_SCHEMA_VERSION,
        "Migrated corpus index schema"
    );
    Ok(())
}

fn read_meta(index_dir: &Path) -> Result<IndexMeta> {
    let path = meta_path(index_dir);
    let content = std::fs::read_to_string(&path).map_err(|e| {
        Error::IndexNotFound(format!("Missing metadata at {}: {e}", path.display()))
    })?;
    serde_json::from_str(&content)
        .map_err(|e| Error::Serialization(format!("Bad index metadata: {e}")))
}

fn write_meta(index_dir: &Path, meta: &IndexMeta) -> Result<()> {
    let path = meta_path(index_dir);
    let json =
        serde_json::to_string_pretty(meta).map_err(|e| Error::Serialization(e.to_string()))?;
    std::fs::write(&path, json)?;
    Ok(())
}

const CHUNKS_TABLE: &str = "chunks";

impl CorpusIndex {
    /// Open an existing LanceDB index.
    ///
    /// Lazily migrates pre-v2 indexes to add the code-intelligence
    /// columns. Migration is idempotent — guarded by the schema_version
    /// field in `_corpus_meta.json` — so subsequent opens are cheap.
    pub async fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::IndexNotFound(path.display().to_string()));
        }

        let mut meta = read_meta(path)?;

        let db = lancedb::connect(path.to_str().unwrap())
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        let table = db
            .open_table(CHUNKS_TABLE)
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        if meta.schema_version < CURRENT_INDEX_SCHEMA_VERSION {
            migrate_schema(&table, meta.schema_version).await?;
            meta.schema_version = CURRENT_INDEX_SCHEMA_VERSION;
            let _ = write_meta(path, &meta);
        }

        Ok(Self {
            db,
            table,
            corpus_id: meta.corpus_id,
            embedding_dimensions: meta.embedding_dimensions,
        })
    }

    // ── Info ───────────────────────────────────────────────

    /// Return metadata about this index.
    pub async fn info(&self) -> Result<IndexInfo> {
        let index_dir = Path::new(self.db.uri());
        let meta = read_meta(index_dir)?;
        let chunk_count = self.chunk_count().await?;
        let index_size_bytes = dir_size(index_dir);

        let chunk_range = if meta.is_shard {
            match (meta.chunk_range_start, meta.chunk_range_end) {
                (Some(s), Some(e)) => Some(ChunkRange::new(s, e)),
                _ => None,
            }
        } else {
            None
        };

        // Resolve the effective `query_sharing` value here so
        // downstream consumers (capability publisher, UI) get a
        // single boolean they can trust — the Option/fallback logic
        // lives in one place.
        let query_sharing = meta.query_sharing.unwrap_or(meta.mesh_sharing);
        // Resolve `dedup_by_source` to a single boolean the runtime reads
        // off `IndexInfo` without re-resolving the recipe. `None` (legacy
        // index) → false (baseline retrieval).
        let dedup_by_source = meta.dedup_by_source.unwrap_or(false);
        // Same resolution shape for `personal_scope`: legacy → false.
        let personal_scope = meta.personal_scope.unwrap_or(false);
        // `source_path` is only set by the code-ingest pipeline
        // (`CorpusIndex::set_source_path`, called from `sovereign
        // code index`) — every other ingest path leaves it `None`.
        // That makes it the authoritative signal for "is this a
        // code corpus?" without needing a schema migration on
        // already-written `_corpus_meta.json` files.
        // Prefer the explicit `kind` written at ingest time. Fall back
        // to source_path-based derivation for indexes written before
        // the field existed (Some → Code, None → Knowledge).
        let kind = meta.kind.unwrap_or_else(|| {
            if meta.source_path.is_some() {
                crate::types::CorpusKind::Code
            } else {
                crate::types::CorpusKind::Knowledge
            }
        });
        Ok(IndexInfo {
            corpus_id: meta.corpus_id,
            corpus_name: meta.corpus_name,
            path: index_dir.to_path_buf(),
            chunk_count,
            index_size_bytes,
            created_at: meta.created_at,
            last_updated: meta.last_updated,
            embedding_model: meta.embedding_model,
            embedding_dimensions: meta.embedding_dimensions,
            mesh_sharing: meta.mesh_sharing,
            query_sharing,
            dedup_by_source,
            personal_scope,
            is_shard: meta.is_shard,
            chunk_range,
            parent_corpus_id: meta.parent_corpus_id,
            chunks_expected: meta.chunks_expected,
            resume_from: meta.resume_from,
            enrichment_enabled: meta.enrichment_enabled,
            enriched_chunks: meta.enriched_chunks,
            source_version: meta.source_version,
            update_manifest_url: meta.update_manifest_url,
            kind,
            vector_index_built: meta.vector_index_built,
            canonical_fingerprint: meta.canonical_fingerprint,
            total_shards: meta.total_shards,
            processed_shards: meta.processed_shards,
            mutable_merge: meta.mutable_merge,
            stream: meta.stream,
            display: meta.display,
        })
    }

    /// Return the number of chunks in the index.
    pub async fn chunk_count(&self) -> Result<u64> {
        let count = self
            .table
            .count_rows(None)
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(count as u64)
    }

    // ── Health-check helpers ───────────────────────────────

    /// Number of documents in the FTS index.
    /// Falls back to `chunk_count()` if the FTS index is unavailable.
    pub async fn fts_doc_count(&self) -> Result<u64> {
        // LanceDB exposes FTS through query; we count via a broad wildcard search
        // that matches everything.  We use the chunk_count as a cheap fallback.
        // A proper Tantivy row count would require internal API access — for now
        // we use a sampling approach: perform a full vector-free scan limited to
        // a very large number and trust LanceDB to return all rows.
        // The simplest reliable proxy: use count_rows with no filter (same as chunk_count).
        self.chunk_count().await
    }

    // ── Access for sharding module ────────────────────────

    /// Get the LanceDB connection.
    pub fn connection(&self) -> &lancedb::Connection {
        &self.db
    }

    /// Get the chunks table.
    pub fn table(&self) -> &lancedb::Table {
        &self.table
    }

    /// Get the embedding dimensions.
    pub fn embedding_dim(&self) -> usize {
        self.embedding_dimensions
    }

    /// Set shard metadata.
    pub fn set_shard_meta(&self, chunk_range: ChunkRange) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.is_shard = true;
        meta.chunk_range_start = Some(chunk_range.start_id);
        meta.chunk_range_end = Some(chunk_range.end_id);
        write_meta(index_dir, &meta)
    }

    /// Record the filesystem source path for this corpus. Called by the
    /// ingest pipeline for code corpora so the `sovereign code watch`
    /// subcommand (and any future re-index caller) can find the
    /// original directory without re-parsing the recipe.
    pub fn set_source_path(&self, path: &Path) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.source_path = Some(path.to_string_lossy().into_owned());
        write_meta(index_dir, &meta)
    }

    /// Return the recorded source path (absolute) for this corpus, if any.
    /// Used by the `sovereign code watch` subcommand.
    pub fn source_path(&self) -> Option<PathBuf> {
        let index_dir = Path::new(self.db.uri());
        read_meta(index_dir)
            .ok()
            .and_then(|m| m.source_path.map(PathBuf::from))
    }

    /// Stamp the corpus kind + parent (catalog) corpus id onto the
    /// on-disk meta. Called by the ingest pipeline immediately after
    /// `create_or_resume_with_sharing` so that `info()` reports the
    /// correct kind and the search layer can partition catalog hits.
    /// Both args are optional — passing `None` for either leaves the
    /// existing value unchanged. Errors when meta is missing or
    /// malformed.
    pub fn set_kind_and_parent(
        &self,
        kind: Option<crate::types::CorpusKind>,
        parent_corpus_id: Option<&str>,
    ) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        if let Some(k) = kind {
            meta.kind = Some(k);
        }
        if let Some(p) = parent_corpus_id {
            meta.parent_corpus_id = Some(p.to_string());
        }
        write_meta(index_dir, &meta)
    }

    /// Stamp the `mutable_merge` policy from the recipe (or the
    /// first input shard, in the merge case) onto this index's
    /// `_corpus_meta.json`. `None` clears the policy back to the
    /// content-hash default; `Some(...)` opts the next merge into
    /// the chosen reconciliation rule.
    pub fn set_mutable_merge(
        &self,
        policy: Option<crate::recipe::MutableMergePolicy>,
    ) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.mutable_merge = policy;
        write_meta(index_dir, &meta)
    }

    /// Stamp the `[retrieval] dedup_by_source` flag from the recipe onto
    /// this index's `_corpus_meta.json`. A post-create meta stamp (mirrors
    /// `set_mutable_merge` / `set_display`) so no `create_*` signature
    /// needs threading. Read back by `installed_indexes()` into
    /// `IndexInfo::dedup_by_source`.
    pub fn set_dedup_by_source(&self, dedup_by_source: bool) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.dedup_by_source = Some(dedup_by_source);
        write_meta(index_dir, &meta)
    }

    /// Stamp the `[retrieval] personal_scope` flag from the recipe onto
    /// this index's `_corpus_meta.json` (mirrors `set_dedup_by_source`).
    /// Read back by `installed_indexes()` into
    /// `IndexInfo::personal_scope`, which the runtime's personal-scope
    /// retrieval filter consults. Also called by the watched-folder
    /// manager to backfill corpora created before the field existed.
    pub fn set_personal_scope(&self, personal_scope: bool) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.personal_scope = Some(personal_scope);
        write_meta(index_dir, &meta)
    }

    /// Read the stamped `personal_scope` value. `None` = never stamped
    /// (legacy index). Lets the backfill path distinguish "explicitly
    /// false" from "predates the field" without rewriting meta.
    pub fn personal_scope(&self) -> Option<bool> {
        let index_dir = Path::new(self.db.uri());
        read_meta(index_dir).ok().and_then(|m| m.personal_scope)
    }
}

/// Backfill helper for indexes created before the `personal_scope`
/// meta field existed: stamp `index_dir`'s `_corpus_meta.json` IFF the
/// field was never written. Pure meta I/O — no LanceDB open — so the
/// watched-folder manager can run it across every registered corpus at
/// daemon boot for pennies. Returns `true` when a stamp was written,
/// `false` when the meta already carried an explicit value.
pub fn backfill_personal_scope(index_dir: &Path, personal_scope: bool) -> Result<bool> {
    let mut meta = read_meta(index_dir)?;
    if meta.personal_scope.is_some() {
        return Ok(false);
    }
    meta.personal_scope = Some(personal_scope);
    write_meta(index_dir, &meta)?;
    Ok(true)
}

/// Backfill helper for indexes created before their recipe emitted a
/// `[display]` block: stamp `index_dir`'s `_corpus_meta.json` IFF no
/// display metadata was ever written. Never clobbers an existing
/// stamp — ingest-time `[display]` and any explicit `set_display`
/// stay authoritative. The display category is load-bearing beyond
/// UI: `is_tiered_category` gates the tiered retrieval surface
/// (RAPTOR briefings + entity-PPR rerank) on it, so a missing stamp
/// silently exempts a corpus from entity-aware retrieval. Returns
/// `true` when a stamp was written.
pub fn backfill_display(index_dir: &Path, display: crate::recipe::DisplayMeta) -> Result<bool> {
    let mut meta = read_meta(index_dir)?;
    if meta.display.is_some() {
        return Ok(false);
    }
    meta.display = Some(display);
    write_meta(index_dir, &meta)?;
    Ok(true)
}

impl CorpusIndex {

    /// Return the `mutable_merge` policy stamped on this index, if
    /// any. Used by `merge_shards` to decide which dedupe branch to
    /// run.
    pub fn mutable_merge(&self) -> Option<crate::recipe::MutableMergePolicy> {
        let index_dir = Path::new(self.db.uri());
        read_meta(index_dir).ok().and_then(|m| m.mutable_merge)
    }

    /// Stamp the stream-axis block onto this index's
    /// `_corpus_meta.json`. Move 5 Stage 2; called by
    /// `sovereign corpus stream-axes` to backfill the block for
    /// installed corpora that lack it, and (eventually) by the
    /// ingest path at install time.
    pub fn set_stream(&self, axes: crate::stream_axes::StreamAxes) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.stream = Some(axes);
        write_meta(index_dir, &meta)
    }

    /// Read the stream-axis block from this index's `_corpus_meta.json`.
    /// `None` if the block was never written (legacy index, or
    /// install path that pre-dates Move 5).
    pub fn stream(&self) -> Option<crate::stream_axes::StreamAxes> {
        let index_dir = Path::new(self.db.uri());
        read_meta(index_dir).ok().and_then(|m| m.stream)
    }

    /// Stamp the recipe's `[display]` block onto this index's
    /// `_corpus_meta.json`. Pure UI metadata — drives Atlas View
    /// rail grouping and "From your conversations" prompt labels.
    /// `None` clears any previously-stamped value.
    pub fn set_display(&self, display: Option<crate::recipe::DisplayMeta>) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.display = display;
        write_meta(index_dir, &meta)
    }

    /// Move 6 P5.b/c: read the per-corpus opt-in flag for the
    /// post-update incremental atlas hook. `false` (or absent) means
    /// `CorpusUpdater::apply_update` skips the hook entirely.
    pub fn atlas_incremental_enabled(&self) -> bool {
        let index_dir = Path::new(self.db.uri());
        read_meta(index_dir)
            .ok()
            .and_then(|m| m.atlas_incremental_enabled)
            .unwrap_or(false)
    }

    /// Move 6 P5.b/c: stamp the per-corpus opt-in flag. Run
    /// `sovereign atlas migrate-ids` first on the same corpus —
    /// flipping this on against a sequential-id atlas is a no-op
    /// (the hook's pre-flight check rejects it) but the rejection
    /// log line is more useful than a malformed-id silent failure.
    pub fn set_atlas_incremental_enabled(&self, enabled: bool) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.atlas_incremental_enabled = Some(enabled);
        write_meta(index_dir, &meta)
    }

    /// The corpus ID this index belongs to.
    pub fn corpus_id(&self) -> &str {
        &self.corpus_id
    }

    /// Get the index directory path.
    pub fn path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(self.db.uri())
    }

    // ── Table helpers ────────────────────────────────────

    async fn has_table(&self, name: &str) -> bool {
        match self.db.table_names().execute().await {
            Ok(names) => names.iter().any(|n| n == name),
            Err(_) => false,
        }
    }
}

// ─── Free helpers ──────────────────────────────────────────

/// Current unix timestamp in seconds.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Calculate total size of a directory recursively.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = p.metadata() {
                total += m.len();
            }
        }
    }
    total
}

/// Sanitize text for FTS queries — strip characters that cause syntax errors.
fn sanitize_fts_query(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_embedding(direction: &[f32; 4]) -> Vec<f32> {
        direction.to_vec()
    }

    async fn create_test_index(dir: &Path) -> CorpusIndex {
        let db_path = dir.join("test-corpus");
        CorpusIndex::create(
            &db_path,
            "test-corpus",
            "Test Corpus",
            "test-model",
            4,
            false,
            "MIT",
        )
        .await
        .expect("create index")
    }

    fn sample_chunks() -> Vec<(InsertChunk, Vec<f32>)> {
        vec![
            (
                InsertChunk {
                    content: "Rust is a systems programming language".into(),
                    title: Some("Rust Language".into()),
                    url: Some("https://rust-lang.org".into()),
                    metadata: Some(r#"{"source":"docs"}"#.into()),
                    content_hash: None,
                    source_doc_id: Some("https://rust-lang.org".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[1.0, 0.0, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Python is great for machine learning".into(),
                    title: Some("Python ML".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: None,
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.0, 1.0, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "SQLite is an embedded database engine".into(),
                    title: Some("SQLite".into()),
                    url: Some("https://sqlite.org".into()),
                    metadata: Some(r#"{"source":"wiki"}"#.into()),
                    content_hash: None,
                    source_doc_id: Some("https://sqlite.org".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.0, 0.0, 1.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Rust and systems programming go hand in hand".into(),
                    title: Some("Systems Programming".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: None,
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.9, 0.1, 0.0, 0.0]),
            ),
        ]
    }

    #[tokio::test]
    async fn create_insert_and_count() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        assert_eq!(idx.chunk_count().await.unwrap(), 0);

        idx.insert_batch(&sample_chunks()).await.unwrap();

        assert_eq!(idx.chunk_count().await.unwrap(), 4);
    }

    /// Regression for the duplicate-id citation corruption: chunk ids
    /// must be allocated from a monotonic high-water mark, NEVER the row
    /// count. After deleting rows (so the row count falls below the max
    /// id), a fresh insert must hand out ids strictly greater than every
    /// id ever used — not ids 3,4 again. The old `chunk_count()`-based
    /// scheme reused ids here, so `neighbors(id)` became ambiguous and a
    /// citation for one article read back as a different one.
    #[tokio::test]
    async fn insert_ids_never_reused_after_delete() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        idx.insert_batch(&sample_chunks()).await.unwrap(); // ids 1,2,3,4
        assert_eq!(idx.max_chunk_id().await.unwrap(), 4);

        // Drop two rows → row count = 2, but max id is still 4.
        idx.delete_chunks_by_ids(&[1, 2]).await.unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 2);
        assert_eq!(idx.max_chunk_id().await.unwrap(), 4);

        // Insert two more. Count-based allocation would assign 3,4 —
        // colliding with the surviving rows. The high-water scheme must
        // assign 5,6 instead.
        idx.insert_batch(&sample_chunks()[..2]).await.unwrap();
        assert_eq!(
            idx.chunk_count().await.unwrap(),
            4,
            "two survivors + two fresh rows"
        );
        assert_eq!(
            idx.max_chunk_id().await.unwrap(),
            6,
            "fresh ids must advance past the prior max (4), not reuse 3,4"
        );
    }

    /// Build a small multi-doc fixture and assert that
    /// `neighbors(center, 1)` returns the immediately-adjacent
    /// chunks within the same `source_doc_id` and never crosses a
    /// document boundary. This is the contract the desktop reading
    /// surface depends on.
    #[tokio::test]
    async fn neighbors_respects_source_doc_boundary() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        // Two documents — doc-a has 3 chunks (ids 1,2,3), doc-b
        // has 2 chunks (ids 4,5). insert_batch assigns ids
        // base_id+1, base_id+2, ... so the first chunk in a fresh
        // index lands at id=1.
        let mut batch: Vec<(InsertChunk, Vec<f32>)> = Vec::new();
        for (i, content) in ["A0", "A1", "A2"].iter().enumerate() {
            batch.push((
                InsertChunk {
                    content: (*content).into(),
                    title: Some("Doc A".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("doc-a".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[1.0, 0.0, 0.0, 0.0]),
            ));
            let _ = i;
        }
        for content in ["B0", "B1"].iter() {
            batch.push((
                InsertChunk {
                    content: (*content).into(),
                    title: Some("Doc B".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("doc-b".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.0, 1.0, 0.0, 0.0]),
            ));
        }
        idx.insert_batch(&batch).await.unwrap();

        // Middle of doc-a → both neighbors present, both in doc-a.
        let win = idx.neighbors(2, 1).await.unwrap().expect("center exists");
        assert_eq!(win.center.content, "A1");
        assert_eq!(win.prev.len(), 1);
        assert_eq!(win.prev[0].content, "A0");
        assert_eq!(win.next.len(), 1);
        assert_eq!(win.next[0].content, "A2");

        // End of doc-a (id 3 → A2) → prev is A1; next must be
        // empty even though chunk id 4 (B0) exists, because B0 is
        // in doc-b.
        let win = idx.neighbors(3, 1).await.unwrap().expect("center exists");
        assert_eq!(win.center.content, "A2");
        assert_eq!(win.prev.len(), 1);
        assert_eq!(win.prev[0].content, "A1");
        assert!(
            win.next.is_empty(),
            "next must not bleed into doc-b, got {:?}",
            win.next.iter().map(|r| &r.content).collect::<Vec<_>>()
        );

        // Start of doc-b (id 4 → B0) → no prev (doc-b has no
        // earlier chunks), next is B1.
        let win = idx.neighbors(4, 1).await.unwrap().expect("center exists");
        assert_eq!(win.center.content, "B0");
        assert!(win.prev.is_empty());
        assert_eq!(win.next.len(), 1);
        assert_eq!(win.next[0].content, "B1");

        // Missing chunk → None, no panic.
        let absent = idx.neighbors(99, 1).await.unwrap();
        assert!(absent.is_none());
    }

    /// Build an InsertChunk with explicit content + content_hash so
    /// dedupe tests can synthesize the duplication scenarios that
    /// the real ingest pipeline produced.
    fn chunk_with_hash(content: &str, hash: &str) -> (InsertChunk, Vec<f32>) {
        (
            InsertChunk {
                content: content.into(),
                title: None,
                url: None,
                metadata: None,
                content_hash: Some(hash.into()),
                source_doc_id: None,
                source_file: None,
                code: InsertCodeMeta::default(),
                unit_id: None,
            },
            make_embedding(&[1.0, 0.0, 0.0, 0.0]),
        )
    }

    /// Three rows with the same content_hash — all but the lowest
    /// id (which the writer assigns sequentially) must be deleted,
    /// leaving exactly one row.
    #[tokio::test]
    async fn dedupe_collapses_identical_content_hashes() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        // Same hash → we expect rows_after = 1 (id=0 wins).
        idx.insert_batch(&[
            chunk_with_hash("alpha", "h-alpha"),
            chunk_with_hash("alpha", "h-alpha"),
            chunk_with_hash("alpha", "h-alpha"),
        ])
        .await
        .unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 3);

        let report = idx.dedupe_by_content_hash().await.unwrap();
        assert_eq!(report.rows_before, 3);
        assert_eq!(report.rows_after, 1);
        assert_eq!(report.duplicates_deleted, 2);
        assert_eq!(report.unique_hashes_kept, 1);
        assert_eq!(report.hashless_rows_preserved, 0);
        assert!(report.changed());
        assert!((report.dup_fraction() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(idx.chunk_count().await.unwrap(), 1);
    }

    /// Hashless rows (legacy data) must not be deleted — we have no
    /// signal to dedup them and the safe move is to preserve.
    #[tokio::test]
    async fn dedupe_preserves_hashless_rows() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        // Mix: two duplicated hashed rows + two hashless legacy rows.
        idx.insert_batch(&sample_chunks()).await.unwrap(); // 4 hashless
        idx.insert_batch(&[
            chunk_with_hash("dup", "h-dup"),
            chunk_with_hash("dup", "h-dup"),
        ])
        .await
        .unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 6);

        let report = idx.dedupe_by_content_hash().await.unwrap();
        assert_eq!(report.rows_before, 6);
        assert_eq!(report.rows_after, 5);
        assert_eq!(report.duplicates_deleted, 1);
        assert_eq!(report.unique_hashes_kept, 1);
        assert_eq!(report.hashless_rows_preserved, 4);
        assert_eq!(idx.chunk_count().await.unwrap(), 5);
    }

    /// A clean index — every row has a unique content_hash — should
    /// no-op cleanly. The DedupeReport reports zero changes.
    #[tokio::test]
    async fn dedupe_is_a_noop_on_clean_index() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        idx.insert_batch(&[
            chunk_with_hash("a", "h-a"),
            chunk_with_hash("b", "h-b"),
            chunk_with_hash("c", "h-c"),
        ])
        .await
        .unwrap();

        let report = idx.dedupe_by_content_hash().await.unwrap();
        assert_eq!(report.rows_before, 3);
        assert_eq!(report.rows_after, 3);
        assert_eq!(report.duplicates_deleted, 0);
        assert_eq!(report.unique_hashes_kept, 3);
        assert!(!report.changed());
        assert_eq!(report.dup_fraction(), 0.0);
    }

    /// build_indexes() must run the pre-build dedupe pass on first
    /// call, collapsing duplicate-content rows BEFORE the vector
    /// index trains on them. On the second call (resume), the
    /// `chunks_deduped` checkpoint short-circuits the scan so we
    /// don't pay for a no-op pass every time.
    #[tokio::test]
    async fn build_indexes_runs_dedupe_prelude_then_skips_on_resume() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        idx.insert_batch(&[
            chunk_with_hash("dup", "h-dup"),
            chunk_with_hash("dup", "h-dup"),
            chunk_with_hash("uniq", "h-uniq"),
        ])
        .await
        .unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 3);
        assert!(!idx.is_chunks_deduped());

        // First build: should dedupe (3 → 2) and mark deduped.
        idx.build_indexes(false, false, None).await.unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 2);
        assert!(idx.is_chunks_deduped());

        // Inject a fresh duplicate AFTER the marker is set. A second
        // build_indexes() must NOT re-run dedupe — that's the whole
        // point of the checkpoint, and skipping it is what keeps
        // resumes cheap.
        idx.insert_batch(&[chunk_with_hash("dup", "h-dup")])
            .await
            .unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 3);

        idx.build_indexes(false, false, None).await.unwrap();
        // Still 3 — dedupe was skipped because chunks_deduped=true.
        // (A future "force re-dedupe" path would clear the flag
        // first; the default resume case is just to skip.)
        assert_eq!(idx.chunk_count().await.unwrap(), 3);
    }

    /// `list_indexed_content_hashes` returns the deduped set so
    /// embed-side gating works against an honest seed even when the
    /// index itself has duplicates.
    #[tokio::test]
    async fn list_content_hashes_dedupes_in_set() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        idx.insert_batch(&[
            chunk_with_hash("x", "h-x"),
            chunk_with_hash("x", "h-x"), // duplicate
            chunk_with_hash("y", "h-y"),
            chunk_with_hash("z", "h-z"),
        ])
        .await
        .unwrap();

        let hashes = idx.list_indexed_content_hashes().await.unwrap();
        assert_eq!(hashes.len(), 3);
        assert!(hashes.contains("h-x"));
        assert!(hashes.contains("h-y"));
        assert!(hashes.contains("h-z"));
    }

    #[tokio::test]
    async fn search_fts_only() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();
        idx.build_indexes(true, true, None).await.unwrap();

        let results = idx.search(&[], "Rust programming", 10).await.unwrap();
        assert!(!results.is_empty(), "FTS search should return results");
        assert!(
            results[0].content.contains("Rust"),
            "top FTS result should mention Rust"
        );
        assert_eq!(results[0].corpus_id, "test-corpus");
    }

    #[tokio::test]
    async fn search_vector_only() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        // Vector search works without index (brute force on small datasets).
        let query = make_embedding(&[0.95, 0.05, 0.0, 0.0]);
        let results = idx.search(&query, "", 10).await.unwrap();
        assert!(!results.is_empty(), "vector search should return results");
        assert!(
            results[0].content.contains("Rust"),
            "top vector result should be about Rust, got: {}",
            results[0].content
        );
    }

    #[tokio::test]
    async fn info_returns_metadata() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        let info = idx.info().await.unwrap();
        assert_eq!(info.corpus_id, "test-corpus");
        assert_eq!(info.corpus_name, "Test Corpus");
        assert_eq!(info.embedding_model, "test-model");
        assert_eq!(info.embedding_dimensions, 4);
        assert_eq!(info.chunk_count, 4);
        assert!(!info.mesh_sharing);
        assert!(!info.is_shard);
        assert!(info.chunk_range.is_none());
        assert!(info.index_size_bytes > 0);
        assert!(info.created_at > 0);
        assert!(info.last_updated >= info.created_at);
    }

    #[tokio::test]
    async fn open_existing_and_search() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("reopen-corpus");

        // Create and populate.
        {
            let idx = CorpusIndex::create(
                &db_path,
                "reopen-corpus",
                "Reopen Test",
                "test-model",
                4,
                true,
                "Apache-2.0",
            )
            .await
            .unwrap();
            idx.insert_batch(&sample_chunks()).await.unwrap();
            idx.build_indexes(true, true, None).await.unwrap();
        }

        // Re-open and verify.
        let idx = CorpusIndex::open(&db_path).await.unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 4);

        let results = idx.search(&[], "embedded database", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("SQLite"));
        assert_eq!(results[0].corpus_id, "reopen-corpus");
    }

    #[test]
    fn sanitize_fts_strips_special_chars() {
        assert_eq!(sanitize_fts_query("hello world"), "hello world");
        assert_eq!(sanitize_fts_query("hello-world"), "hello world");
        assert_eq!(sanitize_fts_query("(NOT) OR *"), "NOT OR");
        assert_eq!(sanitize_fts_query("  "), "");
        assert_eq!(sanitize_fts_query(""), "");
    }

    #[tokio::test]
    async fn open_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let result = CorpusIndex::open(&dir.path().join("nope")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_chunks_by_title_returns_only_matching_title() {
        // Canonical source-expansion use-case: a corpus with several
        // documents, one of which ("Rust Language") has multiple
        // chunks. Fetching by that title must return exactly those
        // chunks, regardless of query-similarity scoring.
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        let chunks = vec![
            (
                InsertChunk {
                    content: "Rust intro section".into(),
                    title: Some("Rust Language".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("rust-doc".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[1.0, 0.0, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Rust ownership section".into(),
                    title: Some("Rust Language".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("rust-doc".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.9, 0.1, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Python basics".into(),
                    title: Some("Python ML".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("python-doc".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.0, 1.0, 0.0, 0.0]),
            ),
        ];
        idx.insert_batch(&chunks).await.unwrap();

        let results = idx
            .fetch_chunks_by_title("Rust Language", 10)
            .await
            .unwrap();

        assert_eq!(results.len(), 2, "two chunks share the Rust Language title");
        for r in &results {
            assert_eq!(r.title.as_deref(), Some("Rust Language"));
            assert_eq!(r.corpus_id, "test-corpus");
            assert!((r.score - 1.0).abs() < 1e-6, "cohesion pull → score=1.0");
        }
    }

    #[tokio::test]
    async fn fetch_chunks_by_title_respects_limit() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        // Ingest 5 chunks all with the same title.
        let mut chunks = Vec::new();
        for i in 0..5 {
            chunks.push((
                InsertChunk {
                    content: format!("section {i}"),
                    title: Some("Big Doc".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("big".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[1.0, 0.0, 0.0, 0.0]),
            ));
        }
        idx.insert_batch(&chunks).await.unwrap();

        let results = idx.fetch_chunks_by_title("Big Doc", 3).await.unwrap();
        assert_eq!(results.len(), 3, "limit must cap the fetch");
    }

    #[tokio::test]
    async fn fetch_chunks_by_title_escapes_sql_quote() {
        // Defense against injection when a title contains a single
        // quote. Same concern as delete_chunks_by_source_doc.
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        let chunks = vec![(
            InsertChunk {
                content: "content".into(),
                title: Some("Joan's Note".into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some("j".into()),
                source_file: None,
                code: InsertCodeMeta::default(),
                unit_id: None,
            },
            make_embedding(&[1.0, 0.0, 0.0, 0.0]),
        )];
        idx.insert_batch(&chunks).await.unwrap();

        let results = idx.fetch_chunks_by_title("Joan's Note", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title.as_deref(), Some("Joan's Note"));
    }

    #[tokio::test]
    async fn fetch_chunks_by_title_empty_inputs_are_noops() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        assert!(idx.fetch_chunks_by_title("", 10).await.unwrap().is_empty());
        assert!(idx
            .fetch_chunks_by_title("Rust Language", 0)
            .await
            .unwrap()
            .is_empty());
    }

    // ─── Provenance round-trip ────────────────────────────────

    #[tokio::test]
    async fn fresh_index_is_self_initiated_by_default() {
        let dir = tempdir().unwrap();
        let _index = create_test_index(dir.path()).await;
        let index_dir = dir.path().join("test-corpus");
        // Newly-created indexes are SelfInitiated — preserves the
        // legacy auto-resume contract for direct-install paths.
        assert_eq!(read_provenance(&index_dir), CorpusProvenance::SelfInitiated);
    }

    #[tokio::test]
    async fn set_provenance_round_trips_to_disk() {
        let dir = tempdir().unwrap();
        let _index = create_test_index(dir.path()).await;
        let index_dir = dir.path().join("test-corpus");

        set_provenance(&index_dir, CorpusProvenance::PeerPulled).expect("stamp");
        assert_eq!(read_provenance(&index_dir), CorpusProvenance::PeerPulled);

        // Idempotent flip back.
        set_provenance(&index_dir, CorpusProvenance::SelfInitiated).expect("stamp back");
        assert_eq!(read_provenance(&index_dir), CorpusProvenance::SelfInitiated);
    }

    #[test]
    fn read_provenance_returns_default_for_missing_meta() {
        let dir = tempdir().unwrap();
        let nonexistent = dir.path().join("nope");
        assert_eq!(
            read_provenance(&nonexistent),
            CorpusProvenance::SelfInitiated,
            "auto-resume's contract: if in doubt, resume"
        );
    }

    #[test]
    fn read_provenance_handles_meta_without_provenance_field() {
        // A pre-provenance meta on disk (older install) deserializes
        // with the field's serde default = SelfInitiated. Auto-resume
        // continues to re-fire those, which is the intended back-compat.
        let dir = tempdir().unwrap();
        let index_dir = dir.path().join("legacy");
        std::fs::create_dir_all(&index_dir).unwrap();
        let legacy_meta = serde_json::json!({
            "corpus_id": "x",
            "corpus_name": "X",
            "embedding_model": "m",
            "embedding_dimensions": 4,
            "mesh_sharing": true,
            "license": "MIT",
            "created_at": 0,
            "last_updated": 0,
        });
        std::fs::write(
            index_dir.join("_corpus_meta.json"),
            serde_json::to_string(&legacy_meta).unwrap(),
        )
        .unwrap();
        assert_eq!(read_provenance(&index_dir), CorpusProvenance::SelfInitiated);
    }

    #[tokio::test]
    async fn reset_for_resume_flips_built_flags_and_in_progress() {
        // After a "completed" run, repair must put the meta back into a
        // shape that auto-resume / install will treat as work-needed.
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        let index_dir = dir.path().join("test-corpus");

        // Simulate a completed run: all built, ingestion not in progress.
        idx.mark_indexes_built().expect("mark_indexes_built");
        idx.mark_vector_index_built().expect("mark_vector");
        // content_fts_built/title_fts_built default false in our fixture;
        // for a faithful "completed" simulation, write them on directly.
        {
            let mut meta = read_meta(&index_dir).unwrap();
            meta.content_fts_built = true;
            meta.title_fts_built = true;
            meta.ingestion_in_progress = false;
            write_meta(&index_dir, &meta).unwrap();
        }
        let pre = read_meta(&index_dir).unwrap();
        assert!(pre.indexes_built && pre.vector_index_built);
        assert!(pre.content_fts_built && pre.title_fts_built);
        assert!(!pre.ingestion_in_progress);

        idx.reset_for_resume().expect("reset_for_resume");

        let post = read_meta(&index_dir).unwrap();
        assert!(!post.indexes_built);
        assert!(!post.vector_index_built);
        assert!(!post.content_fts_built);
        assert!(!post.title_fts_built);
        assert!(post.ingestion_in_progress);
    }

    #[tokio::test]
    async fn reset_for_drift_recovery_is_alias_for_reset_for_resume() {
        // Legacy callers should see identical on-disk effects.
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        let index_dir = dir.path().join("test-corpus");

        idx.mark_indexes_built().unwrap();
        idx.reset_for_drift_recovery().expect("reset");
        let meta = read_meta(&index_dir).unwrap();
        assert!(!meta.indexes_built);
        assert!(meta.ingestion_in_progress);
    }

    #[tokio::test]
    async fn chunks_by_ids_returns_only_requested_rows() {
        // Subset enrichment use-case: a multi-document corpus with
        // many chunks; only a handful belong to the chapters this
        // run cares about. `chunks_by_ids` must return exactly the
        // requested ids and no more, so the caller doesn't materialise
        // unrelated content. Empty input returns empty without
        // hitting the table.
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        let all = idx.all_chunks_full().await.unwrap();
        assert!(
            all.len() >= 3,
            "fixture insufficient: need ≥3 chunks, got {}",
            all.len()
        );
        let pick: Vec<u64> = all.iter().take(2).map(|c| c.id).collect();

        let got = idx.chunks_by_ids(&pick).await.unwrap();
        let mut got_ids: Vec<u64> = got.iter().map(|c| c.id).collect();
        got_ids.sort_unstable();
        let mut want_ids = pick.clone();
        want_ids.sort_unstable();
        assert_eq!(got_ids, want_ids);

        let empty = idx.chunks_by_ids(&[]).await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn chunks_by_ids_dedupes_input() {
        // Caller may hand in repeated ids (chapter A and chapter B
        // both reference the same chunk). The implementation must
        // dedupe before issuing the query, returning each row at
        // most once.
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        let all = idx.all_chunks_full().await.unwrap();
        let one = all.first().expect("fixture has at least one chunk").id;
        let dupes = vec![one, one, one];

        let got = idx.chunks_by_ids(&dupes).await.unwrap();
        assert_eq!(got.len(), 1, "duplicate ids must collapse to one row");
        assert_eq!(got[0].id, one);
    }

    #[tokio::test]
    async fn chunks_by_source_doc_ids_filters_on_lance() {
        // Move 6 P5.a.1 contract: only chunks whose source_doc_id is
        // in the requested set come back, in any order. Empty input
        // short-circuits without a database round-trip.
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        let one = idx
            .chunks_by_source_doc_ids(&["https://rust-lang.org".to_string()])
            .await
            .unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(
            one[0].source_doc_id.as_deref(),
            Some("https://rust-lang.org")
        );

        let both = idx
            .chunks_by_source_doc_ids(&[
                "https://rust-lang.org".to_string(),
                "https://sqlite.org".to_string(),
            ])
            .await
            .unwrap();
        let mut got: Vec<String> = both
            .iter()
            .filter_map(|c| c.source_doc_id.clone())
            .collect();
        got.sort();
        assert_eq!(got, vec!["https://rust-lang.org", "https://sqlite.org"]);

        let empty = idx.chunks_by_source_doc_ids(&[]).await.unwrap();
        assert!(empty.is_empty(), "empty input must short-circuit");

        let unknown = idx
            .chunks_by_source_doc_ids(&["https://example.invalid".to_string()])
            .await
            .unwrap();
        assert!(unknown.is_empty(), "unknown id returns empty");
    }

    #[tokio::test]
    async fn chunks_by_source_doc_ids_escapes_quotes() {
        // The IN-list builder must double-escape single quotes so a
        // doc_id like `O'Brien` doesn't break the SQL fragment.
        // Smoke-test: send a quote-containing id; expect no panic and
        // a clean empty result (no chunk in the fixture matches).
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();
        let got = idx
            .chunks_by_source_doc_ids(&["O'Brien".to_string()])
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn fetch_chunks_by_title_unknown_title_returns_empty() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        let results = idx.fetch_chunks_by_title("No Such Doc", 10).await.unwrap();
        assert!(results.is_empty());
    }

    // ─── Canonical fingerprint ─────────────────────────────────

    /// Build a canonical with three explicit content_hashes and
    /// confirm the fingerprint is the BLAKE3 of `<sorted hashes
    /// joined by \n>`. This pins the algorithm — if the format
    /// ever changes silently, every existing canonical's
    /// fingerprint advertised over gossip suddenly mismatches.
    #[tokio::test]
    async fn canonical_fingerprint_is_blake3_of_sorted_hashes() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        let mk = |hash: &str, ord: f32| {
            (
                InsertChunk {
                    content: format!("content-{hash}"),
                    title: None,
                    url: None,
                    metadata: None,
                    content_hash: Some(hash.to_string()),
                    source_doc_id: None,
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[ord, 0.0, 0.0, 0.0]),
            )
        };
        // Insert in non-sorted order to prove sorting kicks in.
        idx.insert_batch(&[mk("ccc", 0.3), mk("aaa", 0.1), mk("bbb", 0.2)])
            .await
            .unwrap();

        let fp = idx.compute_canonical_fingerprint().await.unwrap();

        let mut hasher = blake3::Hasher::new();
        for h in ["aaa", "bbb", "ccc"] {
            hasher.update(h.as_bytes());
            hasher.update(b"\n");
        }
        let expected = hasher.finalize().to_hex().to_string();
        assert_eq!(fp, expected, "fingerprint must be blake3 of sorted hashes");
    }

    /// Two indexes with the same content_hash set produce identical
    /// fingerprints regardless of insert order. This is the load-
    /// bearing property: peers that ingested the same data agree.
    #[tokio::test]
    async fn canonical_fingerprint_is_insertion_order_invariant() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let idx_a = create_test_index(dir_a.path()).await;
        let idx_b = create_test_index(dir_b.path()).await;
        let mk = |hash: &str| {
            (
                InsertChunk {
                    content: format!("content-{hash}"),
                    title: None,
                    url: None,
                    metadata: None,
                    content_hash: Some(hash.to_string()),
                    source_doc_id: None,
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.0, 0.0, 0.0, 0.0]),
            )
        };
        idx_a
            .insert_batch(&[mk("aaa"), mk("bbb"), mk("ccc")])
            .await
            .unwrap();
        idx_b
            .insert_batch(&[mk("ccc"), mk("aaa"), mk("bbb")])
            .await
            .unwrap();

        let fp_a = idx_a.compute_canonical_fingerprint().await.unwrap();
        let fp_b = idx_b.compute_canonical_fingerprint().await.unwrap();
        assert_eq!(fp_a, fp_b);
    }

    /// Adding a single new content_hash must change the fingerprint
    /// — that's the whole point. Catches a silent regression where
    /// the hasher isn't actually consuming the new line.
    #[tokio::test]
    async fn canonical_fingerprint_changes_on_new_content() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        let mk = |hash: &str| {
            (
                InsertChunk {
                    content: format!("content-{hash}"),
                    title: None,
                    url: None,
                    metadata: None,
                    content_hash: Some(hash.to_string()),
                    source_doc_id: None,
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_embedding(&[0.0, 0.0, 0.0, 0.0]),
            )
        };
        idx.insert_batch(&[mk("aaa"), mk("bbb")]).await.unwrap();
        let fp_before = idx.compute_canonical_fingerprint().await.unwrap();

        idx.insert_batch(&[mk("ccc")]).await.unwrap();
        let fp_after = idx.compute_canonical_fingerprint().await.unwrap();

        assert_ne!(
            fp_before, fp_after,
            "fingerprint must change with new content"
        );
    }

    /// `compute_and_stamp_fingerprint` writes the value into the
    /// on-disk meta and `info()` surfaces it. End-to-end check
    /// that the round-trip path works.
    #[tokio::test]
    async fn compute_and_stamp_fingerprint_persists_to_meta() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        let chunk = (
            InsertChunk {
                content: "hello".into(),
                title: None,
                url: None,
                metadata: None,
                content_hash: Some("hash-1".into()),
                source_doc_id: None,
                source_file: None,
                code: InsertCodeMeta::default(),
                unit_id: None,
            },
            make_embedding(&[1.0, 0.0, 0.0, 0.0]),
        );
        idx.insert_batch(&[chunk]).await.unwrap();

        let fp = idx.compute_and_stamp_fingerprint().await.unwrap();
        assert!(!fp.is_empty());

        let info = idx.info().await.unwrap();
        assert_eq!(info.canonical_fingerprint, Some(fp.clone()));

        // Stamping again is idempotent — content hasn't changed.
        let fp2 = idx.compute_and_stamp_fingerprint().await.unwrap();
        assert_eq!(fp, fp2);
    }

    /// An index whose rows all have null `content_hash` returns the
    /// empty-input fingerprint (BLAKE3 of zero bytes). Logged as a
    /// warning but not an error; mesh sync degrades gracefully when
    /// every peer falls into this case.
    #[tokio::test]
    async fn canonical_fingerprint_handles_hashless_rows() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        // sample_chunks() leaves content_hash None on every row —
        // exactly the legacy/hashless case.
        idx.insert_batch(&sample_chunks()).await.unwrap();
        let fp = idx.compute_canonical_fingerprint().await.unwrap();
        assert_eq!(fp, blake3::Hasher::new().finalize().to_hex().to_string());
    }
}
