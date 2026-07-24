// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation tiered-retrieval provider — the concrete impl of
//! `corpus_engine::enrichment::tiered::TieredEnrichmentProvider` that
//! the daemon injects into `CorpusEngine` at startup.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md`.
//!
//! Architecture rationale (why this lives in sovereign-tools, not
//! corpus-engine):
//! - `build_raptor_atlas` lives in `sovereign-tools::raptor_atlas`
//!   (corpus-agnostic builder; same one attached docs use).
//! - `SqliteStateStore` lives in `sovereign-store`, which
//!   `sovereign-tools` depends on but `corpus-engine` does not.
//! - corpus-engine therefore owns only the trait + the dispatch
//!   loop; sovereign-tools owns the heavy work + persistence.
//!
//! ## v0 scope (this session)
//!
//! - **Tiny bucket (<8 chunks)**: synthesize a single
//!   `ConvRaptorNodeRow` from `chunk.title` (the conversation title
//!   from the claude.ai export) + mean(chunk_embeddings). No LLM
//!   call. Spec opt-2.
//! - **Non-Tiny buckets (Small/Medium/Large/LongTail)**: call
//!   `build_raptor_atlas` with `Speed::Slow` (Phase A default), convert
//!   each `RaptorNode` to `ConvRaptorNodeRow`, persist via
//!   `save_conv_raptor_nodes`.
//! - **State machine**: write a `conv_skeletons` row stamped `Ready`
//!   when the per-conv pass succeeds, `Failed` if anything errored.
//!
//! ## Deferred to next session
//!
//! - Opt-1 (Fast slot routing for ≤30-chunk convs) — needs an
//!   InferenceProvider wrapper that rewrites `preferred_speed`.
//! - T2 entity graph via `extract_action_atoms` + `entity_graph::build`.
//! - T3 motif extraction + classification.
//! - T3 TextTiling segments.
//!
//! Today's landing gives operators a working baseline: re-run the
//! conv-anthropic install and the `conv_raptor_nodes` table fills up
//! with real per-conversation trees (Slow on every bucket — slower
//! than the optimized budget, but it works end-to-end).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use corpus_engine::enrichment::tiered::{ConvBucket, TieredEnrichmentProvider};
use corpus_engine::error::{Error, Result};
use corpus_engine::index::EnrichmentChunkRow;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{DocumentTypeTag, QuoteSpan, RaptorNode};
// `DocumentTypeTag::Unknown` is the closest neutral tag; conversation
// data does not match any of the per-genre variants (Narrative /
// Argument / Evidence / Chronicle / Technical), and `Unknown`'s
// prompt steering already says "Document" generically. A dedicated
// `Conversation` variant lands next session when the briefing layer
// needs to render conv-specific prompt framing.
use sovereign_store::sqlite::{
    ConvRaptorNodeRow, ConvSkeletonRow, ConvTieredState, SqliteStateStore,
};
use uuid::Uuid;

use crate::raptor_atlas::{build_raptor_atlas, ChunkInput};

/// Concrete provider wiring the corpus-engine dispatch trait to the
/// real RAPTOR builder + SQLite persistence.
///
/// ## Folder-corpus reuse convention
///
/// Watched-folder corpora reuse this provider's persistence shape
/// (`conv_raptor_nodes` / `conv_motifs` / `conv_skeletons`) by calling
/// `enrich_conversation` with `conv_uuid = corpus_id`. The bucket
/// classification (`ConvBucket::classify(chunks.len())`) handles
/// folder size variability — a 5-file folder takes the Tiny synthetic
/// path; a 5000-chunk vault takes the LongTail RAPTOR path. The
/// `FolderTieredProvider` sibling below wraps this with the folder
/// runner from `corpus-engine::enrichment::tiered::run_folder_tiered_enrichment`.
pub struct ConvTieredProvider {
    store: Arc<SqliteStateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl ConvTieredProvider {
    pub fn new(store: Arc<SqliteStateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }

    pub fn into_handle(self) -> Arc<dyn TieredEnrichmentProvider> {
        Arc::new(self)
    }
}

#[async_trait]
impl TieredEnrichmentProvider for ConvTieredProvider {
    async fn enrich_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
        embeddings: Vec<Vec<f32>>,
        bucket: ConvBucket,
    ) -> Result<()> {
        let chunk_count = chunks.len();
        let updated_at = Utc::now().timestamp();
        let title = conv_title_from_chunks(&chunks);

        let result: std::result::Result<Vec<ConvRaptorNodeRow>, Error> = match bucket {
            ConvBucket::Tiny => Ok(synthesize_tiny_node(
                corpus_id,
                conv_uuid,
                &title,
                &chunks,
                &embeddings,
                updated_at,
            )),
            ConvBucket::Small | ConvBucket::Medium | ConvBucket::Large | ConvBucket::LongTail => {
                build_raptor_rows(
                    corpus_id,
                    conv_uuid,
                    &chunks,
                    &embeddings,
                    self.inference.clone(),
                    updated_at,
                )
                .await
            }
        };

        match result {
            Ok(nodes) => {
                if let Err(e) = self
                    .store
                    .save_conv_raptor_nodes(corpus_id, conv_uuid, &nodes)
                    .await
                {
                    persist_state(
                        &self.store,
                        corpus_id,
                        conv_uuid,
                        ConvTieredState::Failed,
                        chunk_count,
                        Some(title.clone()),
                        updated_at,
                    )
                    .await;
                    return Err(Error::Database(format!(
                        "conv_tiered: save_conv_raptor_nodes({corpus_id}, {conv_uuid}): {e}"
                    )));
                }
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Ready,
                    chunk_count,
                    Some(title),
                    updated_at,
                )
                .await;
                Ok(())
            }
            Err(e) => {
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Failed,
                    chunk_count,
                    Some(title),
                    updated_at,
                )
                .await;
                Err(e)
            }
        }
    }
}

async fn persist_state(
    store: &SqliteStateStore,
    corpus_id: &str,
    conv_uuid: &str,
    state: ConvTieredState,
    chunk_count: usize,
    overview: Option<String>,
    updated_at: i64,
) {
    let row = ConvSkeletonRow {
        corpus_id: corpus_id.to_string(),
        conv_uuid: conv_uuid.to_string(),
        state: state.as_str().to_string(),
        skeleton_json: None,
        overview,
        segments_json: None,
        chunk_count: chunk_count as i64,
        updated_at,
    };
    if let Err(e) = store.save_conv_skeleton(&row).await {
        tracing::warn!(
            corpus = corpus_id,
            conv = conv_uuid,
            error = %e,
            state = state.as_str(),
            "conv_tiered: save_conv_skeleton failed"
        );
    }
}

/// Pull a representative conv title for opt-3 (reuse instead of an
/// LLM-generated overview). The threaded_turns chunker writes the
/// truncated first-message-of-conversation into the Lance `title`
/// column on every chunk in the conv, so we just take the first
/// non-empty value.
fn conv_title_from_chunks(chunks: &[EnrichmentChunkRow]) -> String {
    chunks
        .iter()
        .find_map(|c| c.title.as_deref().filter(|s| !s.trim().is_empty()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(untitled conversation)".to_string())
}

/// Opt-2: tiny conversations get a single synthetic node carrying the
/// conv title as `summary` and the mean of member embeddings as both
/// `summary_embedding` and `centroid_embedding`. Briefing layer can
/// still render a signpost — it just won't carry LLM paraphrasing.
fn synthesize_tiny_node(
    corpus_id: &str,
    conv_uuid: &str,
    title: &str,
    chunks: &[EnrichmentChunkRow],
    embeddings: &[Vec<f32>],
    updated_at: i64,
) -> Vec<ConvRaptorNodeRow> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let mean = mean_vector(embeddings);
    let chunk_ids: Vec<u64> = chunks.iter().map(|c| c.id).collect();
    let direct_members_json = serde_json::to_string(&chunk_ids).unwrap_or_else(|_| "[]".into());
    let evidence_json = direct_members_json.clone();
    let children_json = "[]".to_string();
    let quotes_json = "[]".to_string();
    let entities_json = "[]".to_string();
    let row = ConvRaptorNodeRow {
        node_id: Uuid::new_v4().to_string(),
        corpus_id: corpus_id.to_string(),
        conv_uuid: conv_uuid.to_string(),
        level: 0,
        summary: title.to_string(),
        summary_embedding: mean.clone(),
        centroid_embedding: mean,
        children_node_ids_json: children_json,
        direct_member_chunk_ids_json: Some(direct_members_json),
        evidence_chunk_ids_json: evidence_json,
        quote_spans_json: quotes_json,
        primary_entities_json: entities_json,
        cluster_coherence: 1.0,
        created_at: updated_at,
    };
    vec![row]
}

/// Non-Tiny path: call the corpus-agnostic `build_raptor_atlas` then
/// convert each `RaptorNode` to the conv-scoped `ConvRaptorNodeRow`
/// shape.
///
/// Note on `ChunkInput.chunk_id` (u32) vs Lance row `id` (u64): the
/// builder uses `chunk_id` as a position handle internally and to
/// index into `embeddings`. For conv corpora this is the Lance row
/// id, which historically fits in u32 well under the 4G ceiling.
/// On the day a single conv corpus crosses that line, we'll need to
/// widen ChunkInput; not load-bearing today.
async fn build_raptor_rows(
    corpus_id: &str,
    conv_uuid: &str,
    chunks: &[EnrichmentChunkRow],
    embeddings: &[Vec<f32>],
    inference: Arc<dyn InferenceProvider>,
    updated_at: i64,
) -> std::result::Result<Vec<ConvRaptorNodeRow>, Error> {
    let raptor_chunks: Vec<ChunkInput> = chunks
        .iter()
        .map(|c| ChunkInput {
            chunk_id: c.id as u32,
            content: c.content.clone(),
        })
        .collect();

    let nodes = build_raptor_atlas(
        &inference,
        &raptor_chunks,
        embeddings,
        DocumentTypeTag::Unknown,
    )
    .await
    .map_err(|e| {
        Error::Database(format!(
            "conv_tiered: build_raptor_atlas({corpus_id}, {conv_uuid}): {e}"
        ))
    })?;

    let mut rows = Vec::with_capacity(nodes.len());
    for node in nodes {
        rows.push(raptor_node_to_row(node, corpus_id, conv_uuid, updated_at)?);
    }
    Ok(rows)
}

fn raptor_node_to_row(
    node: RaptorNode,
    corpus_id: &str,
    conv_uuid: &str,
    fallback_ts: i64,
) -> std::result::Result<ConvRaptorNodeRow, Error> {
    let children_json = serde_json::to_string(&node.children_node_ids)
        .map_err(|e| Error::Database(format!("children_node_ids serialize: {e}")))?;
    let direct_members_json: Option<String> = if node.direct_member_chunk_ids.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&node.direct_member_chunk_ids)
                .map_err(|e| Error::Database(format!("direct_member_chunk_ids serialize: {e}")))?,
        )
    };
    let evidence_json = serde_json::to_string(&node.evidence_chunk_ids)
        .map_err(|e| Error::Database(format!("evidence_chunk_ids serialize: {e}")))?;
    let quotes_json = serde_json::to_string(&serialize_quote_spans(&node.quote_spans))
        .map_err(|e| Error::Database(format!("quote_spans serialize: {e}")))?;
    let entities_json = serde_json::to_string(&node.primary_entities)
        .map_err(|e| Error::Database(format!("primary_entities serialize: {e}")))?;
    let created_at = if node.created_at.timestamp() > 0 {
        node.created_at.timestamp()
    } else {
        fallback_ts
    };
    Ok(ConvRaptorNodeRow {
        node_id: node.node_id,
        corpus_id: corpus_id.to_string(),
        conv_uuid: conv_uuid.to_string(),
        level: node.level as i64,
        summary: node.summary,
        summary_embedding: node.summary_embedding,
        centroid_embedding: node.centroid_embedding,
        children_node_ids_json: children_json,
        direct_member_chunk_ids_json: direct_members_json,
        evidence_chunk_ids_json: evidence_json,
        quote_spans_json: quotes_json,
        primary_entities_json: entities_json,
        cluster_coherence: node.cluster_coherence as f64,
        created_at,
    })
}

/// Serialize `QuoteSpan` to the same JSON shape attached-doc
/// `raptor_nodes.quote_spans` uses, so a future briefing builder can
/// read both tables with one decoder.
fn serialize_quote_spans(spans: &[QuoteSpan]) -> Vec<serde_json::Value> {
    spans
        .iter()
        .map(|s| {
            serde_json::json!({
                "chunk_id": s.chunk_id,
                "char_start": s.char_start,
                "char_end": s.char_end,
                "text": s.text,
            })
        })
        .collect()
}

fn mean_vector(vectors: &[Vec<f32>]) -> Vec<f32> {
    if vectors.is_empty() {
        return Vec::new();
    }
    let dim = vectors[0].len();
    let mut out = vec![0f32; dim];
    let n = vectors.len() as f32;
    for v in vectors {
        for (i, val) in v.iter().enumerate().take(dim) {
            out[i] += *val;
        }
    }
    for slot in out.iter_mut() {
        *slot /= n;
    }
    out
}

// ─── Folder-corpus tiered provider ──────────────────────────────────
//
// Watched-folder corpora go through the same `conv_*` table shape but
// with `conv_uuid = corpus_id` and motif extraction enabled (the
// briefing layer surfaces motifs alongside RAPTOR signposts). Dispatch
// is via `corpus_engine::enrichment::tiered::run_folder_tiered_enrichment`,
// which collapses all source_doc_ids into one bag before calling
// `enrich_conversation`.

use sovereign_core::conv_tiered::ConvMotifRow;

/// Concrete tiered provider for watched-folder corpora. Persists into
/// the same SQLite tables as `ConvTieredProvider` (conv_raptor_nodes /
/// conv_motifs / conv_skeletons) under `conv_uuid = corpus_id`, plus
/// builds + saves the TF-IDF motif index that conversation provider
/// skips (folder briefings surface motifs as recurring-vocabulary
/// anchors per `TIERED_RETRIEVAL.md`).
pub struct FolderTieredProvider {
    store: Arc<SqliteStateStore>,
    inference: Arc<dyn InferenceProvider>,
    /// Resolves the index directory for a given corpus id. Required
    /// for the generic `_enrichment_state.json` sink so the daemon
    /// (and any restart's stall sweeper) can see progress without the
    /// provider hard-coding `~/.sovereign/indexes/`. Set when
    /// constructed from a daemon that knows its index root; left
    /// `None` in unit tests that don't need durable state.
    index_dir_resolver: Option<Arc<dyn IndexDirResolver>>,
    /// Document-type cue handed to the RAPTOR summarizer. Defaults to
    /// `Unknown` (generic "section-level" summaries); a corpus-specific
    /// retrofit sets this via [`FolderTieredProvider::with_doc_type`] —
    /// e.g. `Argument` for SEP philosophy essays so summaries come out
    /// claim-level. Threaded into `build_folder_artifacts`.
    doc_type: DocumentTypeTag,
}

/// Indirection so the provider can locate the per-corpus index dir
/// without importing `corpus_engine::CorpusEngine` (which already
/// depends on this crate via the conv tiered store).
pub trait IndexDirResolver: Send + Sync {
    fn resolve(&self, corpus_id: &str) -> Option<std::path::PathBuf>;
}

/// Concrete resolver that joins `corpus_id` onto a fixed root —
/// what the daemon installs.
pub struct StaticIndexDirResolver {
    pub indexes_root: std::path::PathBuf,
}

impl IndexDirResolver for StaticIndexDirResolver {
    fn resolve(&self, corpus_id: &str) -> Option<std::path::PathBuf> {
        let canonical = self.indexes_root.join(corpus_id);
        if canonical.exists() {
            Some(canonical)
        } else {
            None
        }
    }
}

impl FolderTieredProvider {
    pub fn new(store: Arc<SqliteStateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self {
            store,
            inference,
            index_dir_resolver: None,
            doc_type: DocumentTypeTag::Unknown,
        }
    }

    /// Wire the per-corpus index-dir resolver so this provider
    /// publishes `_enrichment_state.json` while it runs. Daemons
    /// should always set this; tests can skip it.
    pub fn with_index_dir_resolver(mut self, resolver: Arc<dyn IndexDirResolver>) -> Self {
        self.index_dir_resolver = Some(resolver);
        self
    }

    /// Override the document-type cue handed to the RAPTOR summarizer
    /// (default [`DocumentTypeTag::Unknown`]). Lets a corpus-specific
    /// retrofit ask for the right summary shape — `Argument` for
    /// philosophy yields claim-level summaries rather than generic
    /// section-level ones.
    pub fn with_doc_type(mut self, doc_type: DocumentTypeTag) -> Self {
        self.doc_type = doc_type;
        self
    }

    pub fn into_handle(self) -> Arc<dyn TieredEnrichmentProvider> {
        Arc::new(self)
    }

    /// Stamp the `_enrichment_state.json` for a corpus to the given
    /// phase. Best-effort: a missing resolver or write failure logs
    /// but never short-circuits the enrichment body — the durable
    /// outcome lives in the SQLite store regardless.
    fn stamp_state(
        &self,
        corpus_id: &str,
        phase: corpus_engine::enrichment::state::EnrichmentPhase,
        step_current: u64,
        step_total: u64,
        message: Option<&str>,
    ) {
        let Some(resolver) = self.index_dir_resolver.as_ref() else {
            return;
        };
        let Some(index_dir) = resolver.resolve(corpus_id) else {
            return;
        };
        if let Err(e) = corpus_engine::enrichment::state::EnrichmentStateFile::stamp(
            &index_dir,
            corpus_id,
            Some("folder_tiered"),
            phase,
            step_current,
            step_total,
            message,
        ) {
            tracing::warn!(
                corpus = corpus_id,
                phase = phase.label(),
                error = %e,
                "folder_tiered: enrichment state stamp failed"
            );
        }
    }

    /// Build the per-cluster RAPTOR checkpoint and the
    /// state-file-backed progress sink for one enrich call, when the
    /// daemon wired an index-dir resolver. Returns `(None, None)` for
    /// resolver-less paths (unit tests) — the build path tolerates
    /// missing checkpoints + sinks by skipping the durable bits.
    ///
    /// The checkpoint is keyed **per note** (`conv_uuid`), not per
    /// corpus: in a folder/vault corpus each conversation is enriched by
    /// its own `enrich_conversation` call, and a shared slot would let
    /// each note stomp the previous note's manifest — turning a mid-vault
    /// restart into a full re-run of every already-built note. See
    /// `RaptorCheckpointHandle::at_note`.
    fn build_checkpoint_and_sink(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: &[EnrichmentChunkRow],
        embeddings: &[Vec<f32>],
    ) -> (
        Option<crate::raptor_checkpoint::RaptorCheckpointHandle>,
        Option<Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
    ) {
        let Some(resolver) = self.index_dir_resolver.as_ref() else {
            return (None, None);
        };
        let Some(index_dir) = resolver.resolve(corpus_id) else {
            return (None, None);
        };
        let chunk_ids: Vec<u32> = chunks.iter().map(|c| c.id as u32).collect();
        let embedding_dim = embeddings.first().map(|e| e.len()).unwrap_or(0);
        let input_hash = crate::raptor_checkpoint::RaptorCheckpointHandle::compute_input_hash(
            &chunk_ids,
            embedding_dim,
        );
        let checkpoint = crate::raptor_checkpoint::RaptorCheckpointHandle::at_note(
            &index_dir, conv_uuid, input_hash,
        );
        let sink: Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink> =
            Arc::new(corpus_engine::enrichment::state::StateFileSink::new(
                index_dir,
                corpus_id.to_string(),
                Some("folder_tiered".into()),
            ));
        (Some(checkpoint), Some(sink))
    }

    /// Re-run per-source RAPTOR for only the source_doc_ids supplied.
    /// Called by the watched-folder sweeper after `apply_watched_diff`
    /// lands new chunks: instead of rebuilding the whole vault's
    /// RAPTOR forest (cost grows linearly with vault size), we
    /// re-enrich only the notes whose chunk set changed.
    ///
    /// Cheap-on-unchanged: the RAPTOR checkpoint inside
    /// `enrich_conversation` hashes the input chunk-id set + embedding
    /// dim. When the hash matches the prior run's checkpoint, the
    /// per-cluster LLM summarisation short-circuits (`Resume` rather
    /// than `Fresh`). So a sweeper that fires this for a note that
    /// only had a whitespace edit pays the cost of one Lance read +
    /// one checkpoint compare, no LLM work.
    ///
    /// After per-doc work completes the synthesis pass re-runs so
    /// `vault_themes` reflects the post-edit state.
    pub async fn reenrich_changed_sources(
        &self,
        corpus_id: &str,
        source_doc_ids: &[String],
    ) -> Result<()> {
        use corpus_engine::enrichment::tiered::ConvBucket;
        use corpus_engine::enrichment::tiered::TieredEnrichmentProvider;
        use corpus_engine::index::CorpusIndex;

        if source_doc_ids.is_empty() {
            return Ok(());
        }
        let Some(resolver) = self.index_dir_resolver.as_ref() else {
            tracing::warn!(
                corpus = corpus_id,
                "folder_tiered: reenrich_changed_sources called without index_dir_resolver; skipping"
            );
            return Ok(());
        };
        let Some(index_path) = resolver.resolve(corpus_id) else {
            tracing::warn!(
                corpus = corpus_id,
                "folder_tiered: reenrich_changed_sources could not resolve index dir; skipping"
            );
            return Ok(());
        };

        let index = CorpusIndex::open(&index_path).await.map_err(|e| {
            Error::Database(format!(
                "folder_tiered: open corpus index for incremental reenrich ({corpus_id}): {e}"
            ))
        })?;

        let mut reenriched = 0usize;
        let mut skipped_empty = 0usize;
        for doc_id in source_doc_ids {
            let rows = match index.chunks_for_source_doc_with_embeddings(doc_id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        corpus = corpus_id,
                        doc = %doc_id,
                        error = %e,
                        "folder_tiered: incremental reenrich chunk fetch failed; skipping doc"
                    );
                    continue;
                }
            };
            if rows.is_empty() {
                // Deleted note — wipe its sidecar RAPTOR + themes
                // references will be reconciled next finalize.
                let _ = self
                    .store
                    .delete_conv_raptor_nodes_for_source(corpus_id, doc_id)
                    .await;
                skipped_empty += 1;
                continue;
            }
            let (chunks, embeddings): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
            // Per-FILE units (this is the folder incremental path) —
            // mirror `run_folder_tiered_enrichment`'s choice or an
            // edited 3-chunk note silently downgrades from a real
            // RAPTOR summary to a title-only synthetic node.
            let bucket = ConvBucket::classify_note(chunks.len());
            if let Err(e) = self
                .enrich_conversation(corpus_id, doc_id, chunks, embeddings, bucket)
                .await
            {
                tracing::warn!(
                    corpus = corpus_id,
                    doc = %doc_id,
                    error = %e,
                    "folder_tiered: incremental enrich_conversation failed for source_doc"
                );
                continue;
            }
            reenriched += 1;
        }

        tracing::info!(
            corpus = corpus_id,
            reenriched,
            skipped_empty,
            "folder_tiered: incremental reenrich done; re-running vault synthesis"
        );

        // Re-fire synthesis so vault_themes reflects post-edit state.
        // finalize_corpus tolerates failure (best-effort logging).
        let _ = self.finalize_corpus(corpus_id).await;

        // Run-level terminal stamp. The per-note `enrich_conversation`
        // calls above deliberately leave the shared corpus state
        // NON-terminal (they no longer stamp per-note Complete), so
        // without this an incremental sweep would leave the file parked
        // at a per-note `Persisting` — and the next daemon-boot stall
        // sweep would then wrongly flip the corpus to "stalled". Re-affirm
        // Complete now that the touched notes have settled.
        self.stamp_state(
            corpus_id,
            corpus_engine::enrichment::state::EnrichmentPhase::Complete,
            reenriched as u64,
            (reenriched + skipped_empty) as u64,
            Some(&format!("re-enriched {reenriched} changed notes")),
        );
        Ok(())
    }

    /// Vault-wide RAPTOR synthesis pass. Implementation behind
    /// `finalize_corpus`. Public-ish (crate-private) so unit tests
    /// can exercise the synthesis output without going through the
    /// trait surface.
    ///
    /// Algorithm:
    /// 1. Enumerate all source_doc_ids whose `conv_skeletons.state`
    ///    is Ready for this corpus.
    /// 2. For each, fetch level-0 RAPTOR leaves; flatten into a
    ///    cross-vault input list of `(content, embedding)` pairs.
    ///    Track which source_doc each input came from.
    /// 3. Below the minimum-input-count gate (<8 leaves total across
    ///    the vault), skip — there isn't enough cross-note signal to
    ///    cluster usefully. Empty `vault_themes` is the right answer.
    /// 4. Run `build_raptor_atlas` over the flattened input. The
    ///    builder ascends levels until the cluster count drops to
    ///    `<= 4`; we take the highest level as the "themes".
    /// 5. For each theme node, project its `evidence_chunk_ids`
    ///    (which index back into our flattened input order) through
    ///    the source-doc sidecar to recover the contributing notes.
    /// 6. Persist as `VaultThemeRow`s.
    ///
    /// Returns the number of themes persisted (or 0 on a no-op skip).
    pub(crate) async fn run_vault_synthesis(&self, corpus_id: &str) -> Result<usize> {
        use crate::raptor_atlas::{build_raptor_atlas_with_checkpoint, ChunkInput};
        use sovereign_core::conv_tiered::VaultThemeRow;

        // Bound below which synthesis is pointless. 8 = a vault with
        // <2 notes (typical 4-5 leaves each) or one single
        // many-clustered note — neither has cross-note structure to
        // synthesise.
        const MIN_LEAVES_FOR_SYNTHESIS: usize = 8;

        let source_doc_ids = self
            .store
            .list_ready_source_doc_ids_for_corpus(corpus_id)
            .await
            .map_err(|e| {
                Error::Database(format!(
                    "vault_synthesis: list ready source_doc_ids ({corpus_id}): {e}"
                ))
            })?;
        if source_doc_ids.is_empty() {
            tracing::debug!(
                corpus = corpus_id,
                "vault_synthesis: no Ready conv_skeletons; skipping"
            );
            // Wipe any stale themes from a prior state where docs
            // were ready but have since been removed.
            let _ = self.store.delete_vault_themes_for_corpus(corpus_id).await;
            return Ok(0);
        }

        let mut chunks: Vec<ChunkInput> = Vec::new();
        let mut embeddings: Vec<Vec<f32>> = Vec::new();
        // Sidecar mapping: index into `chunks` -> source_doc_id that
        // contributed it. Used after RAPTOR returns to project
        // evidence_chunk_ids (which are u32 indices in 0..chunks.len())
        // back to the originating notes.
        let mut source_for_input: Vec<String> = Vec::new();
        for doc_id in &source_doc_ids {
            let nodes = match self.store.list_conv_raptor_nodes(corpus_id, doc_id).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(
                        corpus = corpus_id,
                        doc = %doc_id,
                        error = %e,
                        "vault_synthesis: per-doc raptor fetch failed; excluding from synthesis"
                    );
                    continue;
                }
            };
            for node in nodes.iter().filter(|n| n.level == 0) {
                let next_id = chunks.len() as u32;
                chunks.push(ChunkInput {
                    chunk_id: next_id,
                    content: node.summary.clone(),
                });
                embeddings.push(node.summary_embedding.clone());
                source_for_input.push(doc_id.clone());
            }
        }

        if chunks.len() < MIN_LEAVES_FOR_SYNTHESIS {
            tracing::debug!(
                corpus = corpus_id,
                leaves = chunks.len(),
                min = MIN_LEAVES_FOR_SYNTHESIS,
                "vault_synthesis: too few level-0 leaves across vault; skipping"
            );
            let _ = self.store.delete_vault_themes_for_corpus(corpus_id).await;
            return Ok(0);
        }

        tracing::info!(
            corpus = corpus_id,
            notes = source_doc_ids.len(),
            leaves = chunks.len(),
            "vault_synthesis: building cross-note RAPTOR tree"
        );

        // Honest phase for the ~N/20-LLM-call cross-vault build. Without
        // it the label stays frozen on the last stamp ("Finding people,
        // places, and ideas") for the whole synthesis — reading as a
        // wedge — because the skip-already-built loop emits no per-note
        // stamps. The sink below then refines it with live per-cluster
        // progress ("Summarizing sections (17 / 45)").
        self.stamp_state(
            corpus_id,
            corpus_engine::enrichment::state::EnrichmentPhase::RaptorTree,
            0,
            chunks.len() as u64,
            Some(&format!(
                "weaving {} notes into vault themes",
                source_doc_ids.len()
            )),
        );

        // Durable checkpoint for the vault-level synthesis — the same
        // resume-across-restarts guarantee as the per-note RAPTOR
        // checkpoint. This cross-vault build is the single most expensive
        // step and, uncheckpointed, restarts from scratch on every daemon
        // boot; if the process dies mid-synthesis (a tauri reload, an app
        // update, a crash) it never converges and the vault never reaches
        // Complete. A dedicated slot keyed on a reserved pseudo-note id
        // never collides with a real note's checkpoint. The input hash is
        // over the leaf *content* (the `ChunkInput` ids here are ephemeral
        // 0..N indices, not stable chunk ids), so a note edit that changes
        // the leaves invalidates it and rebuilds, while an unchanged vault
        // resumes (or short-circuits) with zero LLM.
        let (checkpoint, sink) = match self
            .index_dir_resolver
            .as_ref()
            .and_then(|r| r.resolve(corpus_id))
        {
            Some(index_dir) => {
                let mut hasher = blake3::Hasher::new();
                for c in &chunks {
                    hasher.update(c.content.as_bytes());
                }
                hasher.update(
                    &(embeddings.first().map(|e| e.len()).unwrap_or(0) as u32).to_le_bytes(),
                );
                let input_hash = hasher.finalize().to_hex().to_string();
                let cp = crate::raptor_checkpoint::RaptorCheckpointHandle::at_note(
                    &index_dir,
                    "__vault_synthesis__",
                    input_hash,
                );
                let sink: Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink> =
                    Arc::new(corpus_engine::enrichment::state::StateFileSink::new(
                        index_dir,
                        corpus_id.to_string(),
                        Some("folder_tiered".into()),
                    ));
                (Some(cp), Some(sink))
            }
            None => (None, None),
        };

        let nodes = build_raptor_atlas_with_checkpoint(
            &self.inference,
            &chunks,
            &embeddings,
            DocumentTypeTag::Unknown,
            checkpoint.as_ref(),
            sink.as_ref(),
            None,
        )
        .await
        .map_err(|e| {
            Error::Database(format!(
                "vault_synthesis: build_raptor_atlas ({corpus_id}): {e}"
            ))
        })?;

        let max_level = nodes.iter().map(|n| n.level).max().unwrap_or(0);
        let now = Utc::now().timestamp();
        let themes: Vec<VaultThemeRow> = nodes
            .iter()
            .filter(|n| n.level == max_level)
            .enumerate()
            .map(|(idx, node)| {
                let mut members: Vec<String> = node
                    .evidence_chunk_ids
                    .iter()
                    .filter_map(|cid| source_for_input.get(*cid as usize).cloned())
                    .collect();
                members.sort();
                members.dedup();
                VaultThemeRow {
                    corpus_id: corpus_id.to_string(),
                    theme_id: format!("theme-{idx:03}"),
                    summary: node.summary.clone(),
                    summary_embedding: node.summary_embedding.clone(),
                    member_source_doc_ids_json: serde_json::to_string(&members)
                        .unwrap_or_else(|_| "[]".to_string()),
                    cluster_coherence: node.cluster_coherence,
                    created_at: now,
                }
            })
            .filter(|t| t.member_source_doc_ids_json != "[]")
            .collect();

        let theme_count = themes.len();
        self.store
            .save_vault_themes(corpus_id, &themes)
            .await
            .map_err(|e| {
                Error::Database(format!(
                    "vault_synthesis: save_vault_themes ({corpus_id}): {e}"
                ))
            })?;
        Ok(theme_count)
    }
}

#[async_trait]
impl TieredEnrichmentProvider for FolderTieredProvider {
    async fn enrich_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
        embeddings: Vec<Vec<f32>>,
        bucket: ConvBucket,
    ) -> Result<()> {
        use corpus_engine::enrichment::state::EnrichmentPhase;
        let chunk_count = chunks.len();
        let updated_at = Utc::now().timestamp();
        // Durable summary-revision loop: consult the correction ledger
        // ONCE up front so every rebuild path (targeted flag, sweep,
        // resume, full) re-applies the user's fix. `correction_hint` is
        // injected into the RAPTOR summary prompt; `force_rebuild` (set
        // only for a freshly-flagged 'pending' correction) wipes the
        // content-hash checkpoint below so the summary actually
        // regenerates. See docs/specs/SUMMARY_REVISION_LOOP.md.
        let active_correction = self
            .store
            .get_active_correction(corpus_id, conv_uuid)
            .await
            .ok()
            .flatten();
        let correction_hint = active_correction
            .as_ref()
            .and_then(|c| c.correction_hint.as_deref());
        let force_rebuild = active_correction
            .as_ref()
            .is_some_and(|c| c.status == "pending");
        // Publish the entry point so the UI flips off the
        // indistinguishable "starting…" state the moment the
        // provider claims the corpus. Without this stamp, daemons
        // that restart mid-Scan look identical to daemons that never
        // dispatched at all.
        self.stamp_state(
            corpus_id,
            EnrichmentPhase::Scanning,
            0,
            chunk_count as u64,
            Some(&format!(
                "loaded {chunk_count} chunks; bucket {}",
                bucket.label()
            )),
        );
        // Folder corpora often have chunks tagged with the source file
        // name as `title`; reuse that as the overview when present,
        // otherwise stamp a stable "Folder: <id>" fallback so the
        // briefing renderer doesn't ship "(untitled conversation)" in
        // a watched-folder context.
        let chunk_title = conv_title_from_chunks(&chunks);
        let overview_title = if chunk_title == "(untitled conversation)" {
            format!("Folder: {corpus_id}")
        } else {
            chunk_title
        };

        let result: std::result::Result<(Vec<ConvRaptorNodeRow>, Vec<ConvMotifRow>), Error> =
            match bucket {
                ConvBucket::Tiny => Ok((
                    synthesize_tiny_node(
                        corpus_id,
                        conv_uuid,
                        &overview_title,
                        &chunks,
                        &embeddings,
                        updated_at,
                    ),
                    Vec::new(),
                )),
                ConvBucket::Small
                | ConvBucket::Medium
                | ConvBucket::Large
                | ConvBucket::LongTail => {
                    // Coarse phase stamp before the LLM-heavy build.
                    // build_atlas_artifacts is one big call today; we
                    // can't surface per-leaf progress without
                    // refactoring it. The Stalled sweeper at daemon
                    // start treats a stuck RaptorLeaves phase as
                    // interrupted, so even this coarse stamp is the
                    // difference between "stuck forever" and "shows
                    // 'interrupted, retry'".
                    self.stamp_state(
                        corpus_id,
                        EnrichmentPhase::RaptorLeaves,
                        0,
                        chunk_count as u64,
                        Some(&format!(
                            "building RAPTOR tree over {chunk_count} chunks ({} bucket)",
                            bucket.label()
                        )),
                    );
                    // Construct the per-cluster checkpoint handle +
                    // progress sink if we know where the index dir is.
                    // The handle is keyed by conv_uuid (per-note slot) and
                    // shaped against the input chunk IDs + embedding dim,
                    // so an unchanged note short-circuits on resume while a
                    // note whose chunks changed invalidates cleanly.
                    let (checkpoint_owned, progress_sink_owned) = self.build_checkpoint_and_sink(
                        corpus_id,
                        conv_uuid,
                        &chunks,
                        embeddings.as_slice(),
                    );
                    // A freshly-flagged ('pending') correction must FORCE a
                    // rebuild: the note's content is unchanged, so the
                    // checkpoint would otherwise short-circuit to the cached
                    // WRONG summary with no LLM call. Wipe it so the summary
                    // regenerates with the hint. ('applied' corrections need
                    // no force — content-changed rebuilds re-inject it, and
                    // unchanged ones already hold the fix.)
                    if force_rebuild {
                        if let Some(cp) = checkpoint_owned.as_ref() {
                            cp.reset();
                        }
                    }
                    let checkpoint_ref = checkpoint_owned.as_ref();
                    let progress_ref = progress_sink_owned.as_ref();
                    build_folder_artifacts(
                        corpus_id,
                        conv_uuid,
                        &chunks,
                        &embeddings,
                        self.inference.clone(),
                        self.doc_type.clone(),
                        updated_at,
                        checkpoint_ref,
                        progress_ref,
                        correction_hint,
                    )
                    .await
                }
            };

        match result {
            Ok((nodes, motifs)) => {
                self.stamp_state(
                    corpus_id,
                    EnrichmentPhase::Persisting,
                    0,
                    nodes.len() as u64,
                    Some(&format!("saving {} RAPTOR nodes", nodes.len())),
                );
                if let Err(e) = self
                    .store
                    .save_conv_raptor_nodes(corpus_id, conv_uuid, &nodes)
                    .await
                {
                    persist_state(
                        &self.store,
                        corpus_id,
                        conv_uuid,
                        ConvTieredState::Failed,
                        chunk_count,
                        Some(overview_title.clone()),
                        updated_at,
                    )
                    .await;
                    // Per-note failure is recorded in this note's
                    // `conv_skeletons.state = Failed`; we do NOT stamp the
                    // shared corpus-level state terminal here (one bad note
                    // among many must not mark the whole corpus Failed).
                    // The runner tallies failures and stamps the run-level
                    // terminal after the loop.
                    return Err(Error::Database(format!(
                        "folder_tiered: save_conv_raptor_nodes({corpus_id}, {conv_uuid}): {e}"
                    )));
                }
                if !motifs.is_empty() {
                    if let Err(e) = self
                        .store
                        .save_conv_motifs(corpus_id, conv_uuid, &motifs)
                        .await
                    {
                        // Best-effort. Motif failure degrades briefing
                        // signposts but the RAPTOR tree already
                        // persisted is the load-bearing retrieval
                        // signal.
                        tracing::warn!(
                            corpus = corpus_id,
                            conv = conv_uuid,
                            error = %e,
                            "folder_tiered: save_conv_motifs failed; continuing without motif index"
                        );
                    }
                }
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Ready,
                    chunk_count,
                    Some(overview_title),
                    updated_at,
                )
                .await;
                // Record the content fingerprint LAST — after the skeleton
                // and nodes have durably persisted `Ready`. A stored hash
                // therefore means "this conv is fully built from exactly
                // this content", which is what the conversation runner's
                // skip-already-built check keys on to avoid re-grinding an
                // unchanged conversation on a chat re-import. Best-effort:
                // a write failure just makes the next import re-enrich this
                // conv (the fail-safe direction — never wrongly skip).
                if let Err(e) = self
                    .store
                    .record_conv_content_hash(corpus_id, conv_uuid, &conv_content_hash(&chunks))
                    .await
                {
                    tracing::warn!(
                        corpus = corpus_id,
                        conv = conv_uuid,
                        error = %e,
                        "folder_tiered: record_conv_content_hash failed; conv will re-enrich on next import"
                    );
                }
                // The pending correction (if any) has now been applied —
                // the corrected summary is persisted. Flip the ledger so
                // later rebuilds don't needlessly force again (the hint is
                // still re-injected regardless via correction_hint). The
                // provider owns this transition because it is the thing
                // that actually applies the fix.
                if force_rebuild {
                    let _ = self
                        .store
                        .set_correction_status(corpus_id, conv_uuid, "applied", Some(updated_at))
                        .await;
                }
                // NOTE: we deliberately do NOT stamp the corpus-level
                // `_enrichment_state.json` to `Complete` here. This
                // provider is called once PER DOCUMENT against a state
                // file that is shared by the WHOLE corpus, so a per-note
                // `Complete` made the file flicker to terminal after
                // every note — the desktop poll caught one of those
                // transient "complete"s and stopped, marking a vault
                // explorable with hundreds of notes still unenriched.
                // The run-level terminal stamp is owned by
                // `run_folder_tiered_enrichment` after the full per-doc
                // loop settles. This note's success is durably recorded
                // in `conv_skeletons.state = Ready` above; the last
                // non-terminal stamp (`Persisting`) keeps the UI showing
                // live activity between notes.
                Ok(())
            }
            Err(e) => {
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Failed,
                    chunk_count,
                    Some(overview_title),
                    updated_at,
                )
                .await;
                // See the save-failure arm above: per-note failure lives
                // in `conv_skeletons.state`, not the shared corpus state.
                // The runner owns the run-level terminal stamp.
                Err(e)
            }
        }
    }

    /// After every per-source `enrich_conversation` for this corpus
    /// has settled, run the vault-wide synthesis pass: cluster the
    /// per-note level-0 RAPTOR summaries into ~10-20 cross-note
    /// themes and persist them to `vault_themes`. The retrieval
    /// briefing surfaces these alongside per-note signposts to give
    /// the synth model "what does my whole vault say about X"
    /// context the per-note view alone doesn't carry.
    ///
    /// Best-effort: a failure here only kills the cross-note briefing
    /// block — the per-note tiered retrieval surface is fully
    /// functional regardless. We log + return Ok unless the failure
    /// is catastrophic enough to indicate a bug worth bubbling.
    async fn finalize_corpus(&self, corpus_id: &str) -> Result<()> {
        match self.run_vault_synthesis(corpus_id).await {
            Ok(theme_count) => {
                tracing::info!(
                    corpus = corpus_id,
                    themes = theme_count,
                    "folder_tiered: vault synthesis complete"
                );
            }
            Err(e) => {
                tracing::warn!(
                    corpus = corpus_id,
                    error = %e,
                    "folder_tiered: vault synthesis failed; cross-note briefing block will be empty until next enrichment"
                );
            }
        }

        // The bench-side typed-extension pass is deliberately NOT run
        // here — it moved to `post_finalize_corpus`, which the runner
        // calls AFTER the terminal `Complete` stamp so it never gates the
        // user-facing "map ready" banner (chat retrieval is unaffected by
        // it). `finalize_corpus` now owns only the load-bearing cross-note
        // synthesis that the briefing block actually depends on.
        Ok(())
    }

    /// Post-terminal, best-effort bench-side work: the typed-extension
    /// pass (RAPTOR leaves + vault_themes → atoms.json). SPAWNED DETACHED
    /// so it returns immediately — by the time the runner calls this the
    /// corpus is already stamped `Complete`, so the "Building the map"
    /// banner has cleared and questions already use the full synthesized
    /// map. typed-extension self-gates on a raptor+themes manifest hash
    /// (so it converges) and is best-effort, so if the detached task is
    /// killed (a tauri reload / app quit) it simply re-runs on the next
    /// enrichment. Skipped when the index dir is unknown (unit tests,
    /// transient bring-up).
    async fn post_finalize_corpus(&self, corpus_id: &str) {
        let Some(index_dir) = self
            .index_dir_resolver
            .as_ref()
            .and_then(|r| r.resolve(corpus_id))
        else {
            tracing::debug!(
                corpus = corpus_id,
                "folder_tiered: deferred typed extension skipped — index dir unknown"
            );
            return;
        };
        let store = self.store.clone();
        let inference = self.inference.clone();
        let corpus = corpus_id.to_string();
        tokio::spawn(async move {
            let atlas_dir = index_dir.join("atlas");
            match crate::typed_extension::run_typed_extension(
                &corpus, &store, &inference, &atlas_dir,
            )
            .await
            {
                Ok(report) => tracing::info!(
                    corpus = %corpus,
                    status = ?report.status,
                    pass_a = report.pass_a_calls,
                    pass_b = report.pass_b_calls,
                    "folder_tiered: deferred typed extension complete"
                ),
                Err(e) => tracing::warn!(
                    corpus = %corpus,
                    error = %e,
                    "folder_tiered: deferred typed extension failed; bench atoms.json stale until next enrichment"
                ),
            }
        });
    }

    /// Incremental re-enrichment for the notes whose chunk set changed,
    /// invoked by the watched-folder sweeper after `apply_watched_diff`
    /// lands a delta. Delegates to the inherent
    /// [`FolderTieredProvider::reenrich_changed_sources`], which rebuilds
    /// only the touched notes' RAPTOR trees (cheap-on-unchanged via the
    /// per-note checkpoint) and re-runs vault synthesis.
    ///
    /// Without this override the trait's default no-op ran, so a note
    /// ADDED or edited in a watched vault got embeddings + chunk_entities
    /// but never a `conv_skeleton`/`conv_raptor_nodes` — it silently never
    /// became a "conversation". This is the wiring that makes newly-added
    /// vault notes actually enrich incrementally.
    async fn reenrich_sources(&self, corpus_id: &str, source_doc_ids: &[String]) -> Result<()> {
        self.reenrich_changed_sources(corpus_id, source_doc_ids)
            .await
    }

    /// Skip-already-built: the folder runner calls this before dispatching
    /// each note so an interrupted vault build resumes at the note it
    /// stopped on instead of re-grinding all the already-enriched ones.
    /// Returns `true` only when the note is durably done and unchanged:
    /// its skeleton reached terminal `Ready` AND the persisted
    /// `chunk_count` still matches the live set. A content edit re-chunks
    /// the note (count changes → rebuild); a freshly-flagged correction
    /// vetoes the skip so the guided re-enrich actually regenerates the
    /// summary. `reset_enrichment_state` does NOT clear `conv_skeletons`,
    /// so a `Ready` row is an authoritative "this note is built" signal.
    async fn note_already_current(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunk_count: usize,
    ) -> bool {
        if let Ok(Some(c)) = self.store.get_active_correction(corpus_id, conv_uuid).await {
            if c.status == "pending" {
                return false;
            }
        }
        match self.store.get_conv_skeleton(corpus_id, conv_uuid).await {
            Ok(Some(sk)) => {
                sk.state.as_str() == ConvTieredState::Ready.as_str()
                    && sk.chunk_count == chunk_count as i64
            }
            _ => false,
        }
    }

    /// Content-hash skip for the conversation runner. Skip iff (a) no
    /// pending correction, (b) the skeleton is terminal `Ready`, and (c)
    /// the content fingerprint stored by the last successful enrichment
    /// matches these freshly-fetched chunks. All three reads are cheap
    /// SQLite lookups; getting a `true` here saves the GliNER NER pass +
    /// the full RAPTOR tree build for this conversation. A missing hash
    /// (conv never enriched, or enriched before this marker existed)
    /// returns false → re-enrich, which is the fail-safe direction.
    async fn note_content_current(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: &[EnrichmentChunkRow],
    ) -> bool {
        if let Ok(Some(c)) = self.store.get_active_correction(corpus_id, conv_uuid).await {
            if c.status == "pending" {
                return false;
            }
        }
        let ready = matches!(
            self.store.get_conv_skeleton(corpus_id, conv_uuid).await,
            Ok(Some(sk)) if sk.state.as_str() == ConvTieredState::Ready.as_str()
        );
        if !ready {
            return false;
        }
        match self.store.get_conv_content_hash(corpus_id, conv_uuid).await {
            Ok(Some(stored)) => stored == conv_content_hash(chunks),
            _ => false,
        }
    }
}

/// Content fingerprint of a conversation/note, used by the conversation
/// runner's skip-already-built check to decide whether a re-import can
/// skip re-enrichment. Hashes chunk TEXT in fetch order (document order),
/// NOT chunk ids — the chunk-id allocator is high-water per corpus, so
/// ids are reallocated on re-import and an id-based hash would never
/// match across imports. Each chunk's content is length-prefixed so the
/// boundary between chunks is unambiguous (`["ab","c"]` and `["a","bc"]`
/// hash differently). Deterministic: identical content always yields the
/// same hash, any content change yields a different one.
fn conv_content_hash(chunks: &[EnrichmentChunkRow]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(chunks.len() as u64).to_le_bytes());
    for c in chunks {
        hasher.update(&(c.content.len() as u64).to_le_bytes());
        hasher.update(c.content.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Folder Non-Tiny path: call the corpus-free `build_atlas_artifacts`
/// (sovereign-tools/src/document_asset.rs Move 3 helper) for both
/// RAPTOR nodes and TF-IDF motif index, then convert each to the
/// conv-scoped row shapes for persistence.
async fn build_folder_artifacts(
    corpus_id: &str,
    conv_uuid: &str,
    chunks: &[EnrichmentChunkRow],
    embeddings: &[Vec<f32>],
    inference: Arc<dyn InferenceProvider>,
    doc_type: DocumentTypeTag,
    updated_at: i64,
    checkpoint: Option<&crate::raptor_checkpoint::RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
    correction_hint: Option<&str>,
) -> std::result::Result<(Vec<ConvRaptorNodeRow>, Vec<ConvMotifRow>), Error> {
    let raptor_chunks: Vec<ChunkInput> = chunks
        .iter()
        .map(|c| ChunkInput {
            chunk_id: c.id as u32,
            content: c.content.clone(),
        })
        .collect();

    let (nodes, motifs) = crate::document_asset::build_atlas_artifacts_with_checkpoint(
        &inference,
        &raptor_chunks,
        embeddings,
        doc_type,
        checkpoint,
        progress,
        correction_hint,
    )
    .await
    .map_err(|e| {
        Error::Database(format!(
            "folder_tiered: build_atlas_artifacts({corpus_id}, {conv_uuid}): {e}"
        ))
    })?;

    let mut node_rows = Vec::with_capacity(nodes.len());
    for node in nodes {
        node_rows.push(raptor_node_to_row(node, corpus_id, conv_uuid, updated_at)?);
    }

    let motif_rows: Vec<ConvMotifRow> = motifs
        .into_iter()
        .map(|m| {
            let occ_json =
                serde_json::to_string(&m.occurrence_chunk_ids).unwrap_or_else(|_| "[]".into());
            ConvMotifRow {
                corpus_id: corpus_id.to_string(),
                conv_uuid: conv_uuid.to_string(),
                term: m.term,
                tf_idf_score: m.tf_idf_score as f64,
                occurrence_chunk_ids_json: occ_json,
                is_distinctive: m.is_distinctive,
            }
        })
        .collect();

    Ok((node_rows, motif_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_chunk(id: u64, content: &str, title: Option<&str>) -> EnrichmentChunkRow {
        EnrichmentChunkRow {
            id,
            content: content.to_string(),
            title: title.map(|s| s.to_string()),
            url: None,
            metadata_raw: None,
            source_doc_id: Some("conv-uuid-x".into()),
        }
    }

    #[test]
    fn title_pulled_from_first_non_empty_chunk_title() {
        let chunks = vec![
            mk_chunk(1, "msg 1", None),
            mk_chunk(2, "msg 2", Some("   ")),
            mk_chunk(3, "msg 3", Some("Real Conv Title")),
            mk_chunk(4, "msg 4", Some("Later Title")),
        ];
        assert_eq!(conv_title_from_chunks(&chunks), "Real Conv Title");
    }

    #[test]
    fn untitled_fallback_when_no_chunk_carries_title() {
        let chunks = vec![mk_chunk(1, "msg", None), mk_chunk(2, "msg", None)];
        assert_eq!(conv_title_from_chunks(&chunks), "(untitled conversation)");
    }

    #[test]
    fn conv_content_hash_is_stable_and_chunk_id_independent() {
        // The whole point of the conversation runner's skip: a re-import
        // reallocates chunk ids (high-water allocator), so the hash MUST
        // ignore ids and key only on text. Same text under different ids
        // → same hash → the conv is recognised as unchanged and skipped.
        let first_import = vec![mk_chunk(1, "alpha", None), mk_chunk(2, "beta", None)];
        let re_import = vec![mk_chunk(9001, "alpha", None), mk_chunk(9002, "beta", None)];
        assert_eq!(
            conv_content_hash(&first_import),
            conv_content_hash(&re_import),
            "identical text under reallocated ids must hash identically"
        );
    }

    #[test]
    fn conv_content_hash_detects_same_length_edit() {
        // The exact edge the content-hash guard exists for: an edited
        // conversation that re-chunks to the SAME count. chunk_count alone
        // would wrongly skip it; the content hash must differ so it
        // re-enriches.
        let before = vec![
            mk_chunk(1, "the cat sat", None),
            mk_chunk(2, "on the mat", None),
        ];
        let after = vec![
            mk_chunk(1, "the cat sat", None),
            mk_chunk(2, "on the RUG", None),
        ];
        assert_eq!(
            before.len(),
            after.len(),
            "same chunk count by construction"
        );
        assert_ne!(
            conv_content_hash(&before),
            conv_content_hash(&after),
            "a same-length content edit must change the hash"
        );
    }

    #[test]
    fn conv_content_hash_length_prefix_disambiguates_boundaries() {
        // Without a per-chunk length prefix, ["ab","c"] and ["a","bc"]
        // would concatenate to the same bytes and collide.
        let split_a = vec![mk_chunk(1, "ab", None), mk_chunk(2, "c", None)];
        let split_b = vec![mk_chunk(1, "a", None), mk_chunk(2, "bc", None)];
        assert_ne!(conv_content_hash(&split_a), conv_content_hash(&split_b));
    }

    #[test]
    fn mean_vector_averages_componentwise() {
        let vs = vec![vec![1.0, 2.0, 3.0], vec![3.0, 4.0, 5.0]];
        let m = mean_vector(&vs);
        assert_eq!(m, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn mean_vector_empty_returns_empty() {
        assert!(mean_vector(&[]).is_empty());
    }

    #[test]
    fn synthesize_tiny_node_uses_title_and_mean() {
        let chunks = vec![
            mk_chunk(10, "hi", Some("Convo About React")),
            mk_chunk(11, "more", None),
        ];
        let embeds = vec![vec![1.0, 0.0], vec![3.0, 0.0]];
        let rows = synthesize_tiny_node(
            "corpus-x",
            "conv-y",
            "Convo About React",
            &chunks,
            &embeds,
            42,
        );
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.summary, "Convo About React");
        assert_eq!(row.summary_embedding, vec![2.0, 0.0]);
        assert_eq!(row.centroid_embedding, vec![2.0, 0.0]);
        assert_eq!(row.level, 0);
        assert_eq!(row.corpus_id, "corpus-x");
        assert_eq!(row.conv_uuid, "conv-y");
        assert_eq!(row.created_at, 42);
        assert!(row.direct_member_chunk_ids_json.is_some());
        // Member chunk ids must round-trip the Lance row ids.
        let parsed: Vec<u64> =
            serde_json::from_str(row.direct_member_chunk_ids_json.as_ref().unwrap()).unwrap();
        assert_eq!(parsed, vec![10u64, 11]);
    }

    #[test]
    fn synthesize_tiny_node_empty_chunks_returns_empty() {
        let rows = synthesize_tiny_node("c", "u", "t", &[], &[], 0);
        assert!(rows.is_empty());
    }
}
