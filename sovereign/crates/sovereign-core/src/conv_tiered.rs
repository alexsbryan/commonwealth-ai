// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation tiered-retrieval types + read trait.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md`.
//!
//! Lives in `sovereign-core` (not `sovereign-store`) so the briefing
//! builder in [`crate::conv_briefing`] and the `Runtime` field can
//! reference the trait without `sovereign-core → sovereign-store →
//! sovereign-core` cyclic dependency. The concrete impl on
//! `SqliteStateStore` lives in `sovereign-store::sqlite` per the
//! existing trait/impl split for `StateStore`.

/// State variant for one conversation's tiered enrichment progress.
/// Mirrors `AssetState` but unblocks string-storage of the tag so
/// the briefing layer can render bare strings without serde dance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvTieredState {
    Pending,
    PartiallyReady,
    MultiHopReady,
    Ready,
    Failed,
}

impl ConvTieredState {
    pub fn as_str(self) -> &'static str {
        match self {
            ConvTieredState::Pending => "Pending",
            ConvTieredState::PartiallyReady => "PartiallyReady",
            ConvTieredState::MultiHopReady => "MultiHopReady",
            ConvTieredState::Ready => "Ready",
            ConvTieredState::Failed => "Failed",
        }
    }
}

/// One row from `conv_skeletons` — per-conversation enrichment state
/// + T2 partial skeleton + T3 overview/segments.
#[derive(Debug, Clone)]
pub struct ConvSkeletonRow {
    pub corpus_id: String,
    pub conv_uuid: String,
    pub state: String,
    pub skeleton_json: Option<String>,
    pub overview: Option<String>,
    pub segments_json: Option<String>,
    pub chunk_count: i64,
    pub updated_at: i64,
}

/// One row from `conv_summary_corrections` — a user-authored revision
/// of a note's RAPTOR summary. Keyed 1:1 with the note
/// (`corpus_id, conv_uuid`). `correction_hint` is re-injected into the
/// RAPTOR summarization prompt on every rebuild so the fix persists
/// ("water over stone"). `status`: `pending` (flagged, not yet
/// re-enriched) → `applied` (guided re-enrich landed). See
/// `docs/specs/SUMMARY_REVISION_LOOP.md`.
#[derive(Debug, Clone)]
pub struct SummaryCorrectionRow {
    pub corpus_id: String,
    pub conv_uuid: String,
    pub correction_hint: Option<String>,
    pub original_summary: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub applied_at: Option<i64>,
}

/// One row from `conv_raptor_nodes` — corpus-namespaced RAPTOR tree
/// node. Pre-serialised JSON blobs match the column types.
#[derive(Debug, Clone)]
pub struct ConvRaptorNodeRow {
    pub node_id: String,
    pub corpus_id: String,
    pub conv_uuid: String,
    pub level: i64,
    pub summary: String,
    pub summary_embedding: Vec<f32>,
    pub centroid_embedding: Vec<f32>,
    pub children_node_ids_json: String,
    pub direct_member_chunk_ids_json: Option<String>,
    pub evidence_chunk_ids_json: String,
    pub quote_spans_json: String,
    pub primary_entities_json: String,
    pub cluster_coherence: f64,
    pub created_at: i64,
    /// Summarization prompt+grammar version that produced `summary`.
    /// Empty = pre-stamping row (T1 P1.3) — stale by definition.
    pub prompt_version: String,
    /// Concrete stem of the model that wrote `summary`. Empty =
    /// pre-stamping row, or a non-LLM synthetic row (note titles).
    pub summarizer_model: String,
}

// `ConvMotifRow` was deleted 2026-08-02 with the folder-path motif
// pass. The `conv_motifs` table it mapped had no reader anywhere in
// the workspace and cost 42.8% of a cold vault build to populate.

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

/// Read-side handle the briefing builder calls to surface
/// conv-tiered enrichment in retrieval prompts. The concrete impl on
/// One vault-wide synthesis theme (`vault_themes` table, added with
/// the vault port). Each row is the result of clustering all
/// per-note RAPTOR cluster summaries in one vault into ~10-20
/// cross-note themes. The briefing layer surfaces these as a
/// "Vault themes" prompt block alongside the per-note conv-tiered
/// briefing — gives the synth model cross-note synthesis context the
/// per-note view alone doesn't carry.
#[derive(Debug, Clone)]
pub struct VaultThemeRow {
    pub corpus_id: String,
    pub theme_id: String,
    pub summary: String,
    /// Raw f32 embedding of the theme summary. Same encoding
    /// (little-endian) as `ConvRaptorNodeRow::summary_embedding`.
    pub summary_embedding: Vec<f32>,
    /// JSON array of `source_doc_id` strings — the notes whose
    /// per-note RAPTOR cluster summaries contributed to this theme.
    /// Stored as JSON because vault note ids are arbitrary path
    /// strings and the briefing fetch path projects them straight
    /// into a `Vec<String>` for the `intersect(hit_source_doc_ids)`
    /// check.
    pub member_source_doc_ids_json: String,
    /// Mean intra-cluster cosine of the per-note summaries that
    /// mapped to this theme. Range [0, 1]. Briefing uses it to rank
    /// themes when more than one matches the hit set.
    pub cluster_coherence: f32,
    pub created_at: i64,
}

/// The one error every unimplemented [`ConvBrowseReader`] method returns.
/// A single decider for the text (§10.6) so a route mapping it to 501 can
/// match one shape, and so the method that refused always names itself.
fn browse_unsupported(method: &str) -> crate::error::Error {
    crate::error::Error::NotImplemented(format!(
        "ConvBrowseReader::{method}: this store carries no conversation-tiered \
         browse surface"
    ))
}

/// The Atlas **browse** reads — the five conversation-tiered queries that
/// exist to render a list, a detail pane or an entity drawer, and that no
/// turn ever runs.
///
/// Split out rather than folded into [`ConvTieredReader`] (ARCH §5.1: a
/// trait past ~8 methods wants a sub-trait shape; the retrieval trait is at
/// seven). The two are different concerns with different callers:
/// `ConvTieredReader` is what the briefing layer reads *while answering a
/// question* — whole-conversation reads, one conv at a time.
/// `ConvBrowseReader` is what a human staring at the corpus needs — paging,
/// substring filtering, cross-corpus rollups, entity aggregation. A store
/// can honestly serve one and not the other.
///
/// It is declared a **supertrait of `ConvTieredReader`** on purpose. That is
/// the whole point of the split: `Runtime::lane_sources` already carries an
/// `Option<Arc<dyn ConvTieredReader>>`, and every serving daemon already
/// builds a `Runtime` — so a route reaches all seven atlas store reads
/// through an object `ServingCore` already holds, with no upcast, no second
/// field on `ServingCore`, and no widening of `StateStore` (which lives in
/// `sovereign-contracts`, *below* this crate, and could not name these row
/// types without dragging eleven of them down with it).
///
/// Every method defaults to [`crate::error::Error::NotImplemented`] naming
/// itself. It never defaults to an empty page: a browse UI renders an empty
/// page as "this corpus has no conversations", which is a different claim
/// from "this store cannot answer that" (ARCH §18.3 — absence is reported,
/// never defaulted).
#[async_trait::async_trait]
pub trait ConvBrowseReader: Send + Sync {
    /// Every corpus that has ever been conv-tiered, with its state
    /// histogram. One tuple per corpus: `(corpus_id, total,
    /// max_updated_at, per_state)`.
    ///
    /// The tuple is the shape `SqliteStateStore` already returns and the
    /// desktop already destructures; naming it is a separate change, not
    /// this one's to smuggle in.
    async fn list_conv_corpora_with_state_buckets(
        &self,
    ) -> crate::error::Result<Vec<(String, u64, i64, Vec<(String, u64)>)>> {
        Err(browse_unsupported("list_conv_corpora_with_state_buckets"))
    }

    /// One page of a corpus's conversations, optionally filtered by
    /// case-insensitive substring on `overview`. Returns the page slice
    /// plus the total matching count (not the page length).
    async fn list_conversations_paginated(
        &self,
        _corpus_id: &str,
        _filter: Option<&str>,
        _offset: u64,
        _limit: u64,
    ) -> crate::error::Result<(Vec<ConvSkeletonRow>, u64)> {
        Err(browse_unsupported("list_conversations_paginated"))
    }

    /// One conversation's skeleton row. `None` = the tiered pass has never
    /// run for this `(corpus_id, conv_uuid)`, which is a fact, not a
    /// refusal.
    async fn get_conv_skeleton(
        &self,
        _corpus_id: &str,
        _conv_uuid: &str,
    ) -> crate::error::Result<Option<ConvSkeletonRow>> {
        Err(browse_unsupported("get_conv_skeleton"))
    }

    /// The active user-authored summary correction for one conversation,
    /// or `None` when the user has never flagged it.
    async fn get_active_correction(
        &self,
        _corpus_id: &str,
        _conv_uuid: &str,
    ) -> crate::error::Result<Option<SummaryCorrectionRow>> {
        Err(browse_unsupported("get_active_correction"))
    }

    /// One entity's footprint inside a corpus — mention/conv/chunk counts,
    /// label breakdown, top convs, co-occurring entities. `co_limit` and
    /// `conv_limit` cap the two tail lists.
    async fn aggregate_entity(
        &self,
        _corpus_id: &str,
        _text: &str,
        _co_limit: usize,
        _conv_limit: usize,
    ) -> crate::error::Result<EntityAggregateRow> {
        Err(browse_unsupported("aggregate_entity"))
    }
}

/// `SqliteStateStore` ships in `sovereign-store::sqlite`.
///
/// Future ports (vault, SEP, corpus-wide RAPTOR) either impl this
/// trait directly OR motivate consolidation into a generic
/// `TieredRetrievalSurface` per the spec's §"Retrieval surface —
/// next session's trait" planning section.
#[async_trait::async_trait]
pub trait ConvTieredReader: ConvBrowseReader {
    async fn list_conv_skeletons_for_corpus(
        &self,
        corpus_id: &str,
        conv_uuids: &[String],
    ) -> crate::error::Result<Vec<ConvSkeletonRow>>;

    async fn list_conv_raptor_nodes(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> crate::error::Result<Vec<ConvRaptorNodeRow>>;

    /// Every RAPTOR node for a corpus at or above `min_level` (0 = all,
    /// incl. leaves). The corpus-wide collapsed-tree pool for query-time
    /// cosine grounding. That reader was `Runtime::apply_raptor_grounding`,
    /// retired 2026-09-07 (ei-5c); the rows reach retrieval as `Summary` atoms
    /// the atlas walk seeds on (`svrn enrich summary-atoms <corpus>`).
    async fn list_corpus_raptor_nodes(
        &self,
        corpus_id: &str,
        min_level: i64,
    ) -> crate::error::Result<Vec<ConvRaptorNodeRow>>;

    /// Newest `created_at` across a corpus's RAPTOR nodes (0 when none) — a
    /// cheap build-version for the `raptor_summaries.lance` freshness gate
    /// (`Runtime::raptor_index_fresh`). The default derives it from
    /// `list_corpus_raptor_nodes`; `SqliteStateStore` overrides it with a
    /// `MAX(created_at)` aggregate so the per-query probe never pays the
    /// full-table decode the grounding scan does.
    async fn corpus_raptor_version(&self, corpus_id: &str) -> crate::error::Result<i64> {
        Ok(self
            .list_corpus_raptor_nodes(corpus_id, 0)
            .await?
            .iter()
            .map(|n| n.created_at)
            .max()
            .unwrap_or(0))
    }

    /// All `chunk_entities` rows for one conversation. Returned in
    /// `(chunk_id ASC, char_start ASC)` order so consumers building
    /// the entity graph see entities in their natural document
    /// position. Empty when GliNER hasn't run yet for this conv.
    async fn list_chunk_entities_for_conv(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
    ) -> crate::error::Result<Vec<ChunkEntityRow>>;

    /// Extraction progress for one corpus, if any extraction has
    /// ever been initiated. `None` when the corpus has never been
    /// processed.
    async fn get_chunk_entity_progress(
        &self,
        corpus_id: &str,
    ) -> crate::error::Result<Option<ChunkEntityProgressRow>>;

    /// All vault-wide synthesis themes for a corpus, ordered by
    /// `cluster_coherence DESC` so the briefing picks the
    /// most-coherent themes first when capping render count. Empty
    /// when the vault-wide synthesis pass has not run yet — the
    /// briefing falls through to per-note signposts only, no panic.
    async fn list_vault_themes_for_corpus(
        &self,
        corpus_id: &str,
    ) -> crate::error::Result<Vec<VaultThemeRow>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store with no conversation-tiered tables at all. It takes every
    /// `ConvBrowseReader` default — which is the whole point of the test.
    struct NoBrowseStore;
    impl ConvBrowseReader for NoBrowseStore {}

    /// The default path REFUSES, and says which method refused.
    ///
    /// The failure this pins is the one that reads as a feature: a default
    /// of `Ok(vec![])` / `Ok(None)` compiles, satisfies every caller, and
    /// renders in the Atlas browser as "this corpus has no conversations"
    /// — a store's inability to answer, laundered into a claim about the
    /// user's data (ARCH §18.3). Watched red on 2026-09-10 by replacing
    /// each default body with the empty value: all four assertions below
    /// fail, and nothing else in the workspace does.
    #[tokio::test]
    async fn browse_defaults_refuse_by_name_rather_than_returning_empty() {
        let store = NoBrowseStore;

        // A list read: not an empty page.
        let err = store
            .list_conv_corpora_with_state_buckets()
            .await
            .expect_err("a store with no conv-tiered tables must refuse, not report zero corpora");
        assert!(
            matches!(err, crate::error::Error::NotImplemented(_)),
            "refusal must be the named NotImplemented variant so a route can map it to 501, got: {err:?}"
        );
        assert!(
            err.to_string()
                .contains("ConvBrowseReader::list_conv_corpora_with_state_buckets"),
            "the refusal must name the method that refused, got: {err}"
        );

        // A paged read: not `(vec![], 0)`, which renders as "no matches".
        let err = store
            .list_conversations_paginated("c", None, 0, 20)
            .await
            .expect_err("paging an unsupported store must refuse, not report an empty page");
        assert!(err
            .to_string()
            .contains("ConvBrowseReader::list_conversations_paginated"));

        // An Option read: `None` here would mean "never enriched", which is
        // a claim about the conversation, not about the store.
        let err = store
            .get_conv_skeleton("c", "conv-1")
            .await
            .expect_err("an unsupported store must refuse, not answer None");
        assert!(err
            .to_string()
            .contains("ConvBrowseReader::get_conv_skeleton"));

        let err = store
            .get_active_correction("c", "conv-1")
            .await
            .expect_err("an unsupported store must refuse, not answer None");
        assert!(err
            .to_string()
            .contains("ConvBrowseReader::get_active_correction"));

        // An aggregate: a zero-count row is the most dangerous default of
        // the five — it looks like a real measurement of an absent entity.
        let err = store
            .aggregate_entity("c", "Borges", 20, 10)
            .await
            .expect_err("an unsupported store must refuse, not report a zero-mention entity");
        assert!(err
            .to_string()
            .contains("ConvBrowseReader::aggregate_entity"));
    }

    /// The supertrait edge is what makes the widening reach a route: a
    /// holder of `Arc<dyn ConvTieredReader>` — which is exactly what
    /// `Runtime::lane_sources.conv_tiered` carries — can call the browse
    /// methods with no upcast. If this stops compiling, the daemon route
    /// has lost its door.
    #[test]
    fn conv_tiered_reader_is_also_a_browse_reader() {
        fn assert_browse<T: ConvTieredReader + ?Sized>() {}
        fn _reaches_browse(r: &dyn ConvTieredReader) -> &dyn ConvBrowseReader {
            r
        }
        assert_browse::<dyn ConvTieredReader>();
    }
}
