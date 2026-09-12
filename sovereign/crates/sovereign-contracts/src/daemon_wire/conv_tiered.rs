// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-tiered rows the daemon answers over HTTP —
//! `GET /internal/atlas/conv/{corpus}/entity-aggregate` and
//! `.../chunk-entity-progress` (sovereign-mesh atlas_http). They are wire
//! DTOs, so they live here; `sovereign_core::conv_tiered` re-exports them
//! at the historical path (the lib.rs:65 pattern).

/// One per-chunk entity mention from the GliNER NER pass. Persisted
/// in the `chunk_entities` table (migration:
/// `run_chunk_entities_migration`). One chunk can produce many
/// rows; same (text, label) within a chunk is deduped.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChunkEntityRow {
    pub corpus_id: String,
    pub chunk_id: u64,
    pub text: String,
    pub label: String,
    pub char_start: i64,
    pub char_end: i64,
    pub score: f64,
    pub conv_uuid: Option<String>,
    pub extracted_at: i64,
}

/// Per-corpus extraction progress snapshot. Drives the CLI's "n / N
/// chunks processed" line + the desktop's "entity extraction
/// running" badge (future).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChunkEntityProgressRow {
    pub corpus_id: String,
    pub chunks_processed: i64,
    pub chunks_total: i64,
    pub mentions_extracted: i64,
    pub last_chunk_id: Option<i64>,
    pub started_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
    pub state: String,
    pub model_id: Option<String>,
    pub threshold: Option<f64>,
    pub labels_json: Option<String>,
    pub error_msg: Option<String>,
}

/// Per-label mention count inside one `EntityAggregateRow`. Splits
/// the surface-form collapse (Person:"Swift" vs Organization:"SWIFT")
/// so the drawer can show typed breakdown without merging.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityLabelCount {
    pub label: String,
    pub count: i64,
}

/// One conv that mentioned the queried entity, with mention count.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityConvHit {
    pub conv_uuid: String,
    pub mention_count: i64,
}

/// One entity that co-appears with the queried entity inside the
/// same chunk. `shared_chunk_count` is the number of distinct chunks
/// the two share — high values mean tight topical bond. Same
/// `(text, label)` collapse rules as the seed entity apply.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CoOccurringEntity {
    pub text: String,
    pub label: String,
    pub shared_chunk_count: i64,
}

/// Aggregate view of one entity's footprint inside a corpus. Powers
/// the desktop's Atlas-view entity drawer — click an
/// `entity-chip`, get this back. The seed entity is matched
/// case-insensitively by surface form (`COLLATE NOCASE`) to fold
/// per-mention casing variance; the label breakdown disambiguates
/// homonyms by type.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityAggregateRow {
    pub corpus_id: String,
    /// Canonical display form — first variant seen in the corpus.
    pub text: String,
    /// Per-label mention counts. Two rows = a homonym (e.g.
    /// Person:Swift + Organization:SWIFT).
    pub labels: Vec<EntityLabelCount>,
    pub mention_count: i64,
    /// Distinct conversations mentioning the entity. NULL `conv_uuid`
    /// rows (non-conv corpora) count as zero towards this number.
    pub conv_count: i64,
    /// Distinct chunks mentioning the entity. Always >= mention_count
    /// when the same chunk surfaces the entity twice.
    pub chunk_count: i64,
    /// Top convs by mention count, descending.
    pub top_convs: Vec<EntityConvHit>,
    /// Top entities co-appearing with the seed inside the same
    /// chunks, descending by `shared_chunk_count`.
    pub co_occurring: Vec<CoOccurringEntity>,
}
