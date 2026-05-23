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
                corpus_id, conv_uuid, &title, &chunks, &embeddings, updated_at,
            )),
            ConvBucket::Small
            | ConvBucket::Medium
            | ConvBucket::Large
            | ConvBucket::LongTail => {
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
        let rows =
            synthesize_tiny_node("corpus-x", "conv-y", "Convo About React", &chunks, &embeds, 42);
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
        let parsed: Vec<u64> = serde_json::from_str(
            row.direct_member_chunk_ids_json.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(parsed, vec![10u64, 11]);
    }

    #[test]
    fn synthesize_tiny_node_empty_chunks_returns_empty() {
        let rows = synthesize_tiny_node("c", "u", "t", &[], &[], 0);
        assert!(rows.is_empty());
    }
}
