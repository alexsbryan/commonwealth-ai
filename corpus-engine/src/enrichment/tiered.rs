//! Conversation tiered-retrieval enrichment runner (Phase B port).
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md`.
//!
//! Replaces the legacy field-model atlas enrichment for conversation
//! corpora. Reads chunks from the corpus's Lance index, groups by
//! `source_doc_id` (= `conv_uuid`), and runs the Phase A tiered
//! pipeline per-conversation:
//!
//! - **T2** lean entity extraction + action atoms + per-conv entity
//!   graph (for PPR multi-hop retrieval).
//! - **T3** RAPTOR cluster tree + TF-IDF motifs + TextTiling segments
//!   + overview (overview reuses `metadata.title` from the source
//!   export per v0 opt-3).
//!
//! Writes go to the SQLite sidecar tables `conv_skeletons`,
//! `conv_raptor_nodes`, `conv_motifs` (migration:
//! `sovereign-store/src/migrations.rs::run_conv_tiered_migration`).
//!
//! ## v0 status (Move 3 step 3)
//!
//! - Reads Lance chunks, groups by `source_doc_id`, classifies each
//!   conversation into a size bucket per the spec's "Performance
//!   budget" section, and emits per-bucket stats to tracing.
//! - **Persistence and per-conv RAPTOR / entity-graph / motif calls
//!   are deferred to step 4 / 5.** Those land via a
//!   `TieredEnrichmentProvider` trait wired through `CorpusEngine`
//!   (similar pattern to `InferenceFn`) so corpus-engine doesn't take
//!   a cyclic dep on `sovereign-tools` where `build_raptor_atlas`
//!   lives.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::Result;
use crate::index::{CorpusIndex, EnrichmentChunkRow};
use crate::recipe::Recipe;

/// Shared handle to a `TieredEnrichmentProvider` impl. `Arc<dyn>` so
/// the daemon can pass one instance through `CorpusEngine` without
/// taking ownership.
pub type TieredProviderHandle = Arc<dyn TieredEnrichmentProvider>;

/// Shared handle for the per-chunk NER extractor (GliNER today, via
/// `sovereign-tools::gliner_ner`). Optional second hook fired by the
/// tiered runner ahead of the heavy `TieredEnrichmentProvider` call
/// — runs the cheap CPU-only NER pass first so the chunk_entities
/// table populates even when the LLM-side enrichment fails or is
/// killed mid-run. `None` falls back to RAPTOR-derived entities only.
pub type ChunkEntityExtractorHandle = Arc<dyn ChunkEntityExtractor>;

/// Per-chunk named-entity extractor. corpus-engine declares the
/// trait so the dispatch loop can fire it per-conversation;
/// sovereign-tools owns the concrete impl (where the `gline-rs` dep
/// lives) and the SqliteStateStore persistence path.
///
/// One call per conversation: implementor batches chunks internally
/// for throughput. Returns the count of mentions persisted so the
/// runner can surface a "extracted N entities" log line.
#[async_trait::async_trait]
pub trait ChunkEntityExtractor: Send + Sync {
    async fn extract_for_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
    ) -> Result<usize>;
}

/// Provider trait for the heavy tiered-enrichment work
/// (`build_raptor_atlas`, entity-graph extraction, motif
/// classification, persistence). corpus-engine knows about the trait
/// but ships no concrete impl — sovereign-tools provides one (where
/// `build_raptor_atlas` lives) and injects it into `CorpusEngine`
/// before ingest runs, mirroring the existing `InferenceFn`
/// inversion.
///
/// The provider owns the entire per-conversation work unit including
/// SQLite persistence to the `conv_skeletons` / `conv_raptor_nodes` /
/// `conv_motifs` sidecar tables; corpus-engine just dispatches one
/// call per non-`Tiny` conversation.
///
/// **`Tiny` conversations bypass the provider entirely** — the
/// dispatch runner persists a synthetic single-node entry directly
/// (opt-2 in the spec performance budget).
#[async_trait::async_trait]
pub trait TieredEnrichmentProvider: Send + Sync {
    async fn enrich_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
        embeddings: Vec<Vec<f32>>,
        bucket: ConvBucket,
    ) -> Result<()>;
}

/// Size bucket for a single conversation; drives the slot routing
/// (Fast vs Slow) and the skip-RAPTOR opt-2 decision per the spec
/// "Performance budget" section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConvBucket {
    /// `< 8` chunks. Opt-2: persist a synthetic single node from the
    /// conv title; no LLM call.
    Tiny,
    /// `8..=30` chunks. Opt-1: route summarization through `Speed::Fast`
    /// (9B model). Batched 8-at-a-time per opt-4 when v1 lands.
    Small,
    /// `31..=100` chunks. Fast slot per leaf, ~1-3 LLM calls.
    Medium,
    /// `101..=300` chunks. Fast slot for leaves, Slow for root.
    Large,
    /// `> 300` chunks. Full Phase A treatment, Slow slot throughout.
    LongTail,
}

impl ConvBucket {
    pub fn classify(chunk_count: usize) -> Self {
        match chunk_count {
            0..=7 => ConvBucket::Tiny,
            8..=30 => ConvBucket::Small,
            31..=100 => ConvBucket::Medium,
            101..=300 => ConvBucket::Large,
            _ => ConvBucket::LongTail,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ConvBucket::Tiny => "tiny",
            ConvBucket::Small => "small",
            ConvBucket::Medium => "medium",
            ConvBucket::Large => "large",
            ConvBucket::LongTail => "long_tail",
        }
    }
}

/// Per-bucket aggregate stats reported up the call stack and through
/// tracing so operators can see the work shape before steps 4/5 fire
/// the actual LLM calls.
#[derive(Debug, Clone, Default)]
pub struct TieredDispatchPlan {
    pub corpus_id: String,
    pub total_conversations: usize,
    pub total_chunks: usize,
    pub per_bucket: BTreeMap<ConvBucket, BucketSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct BucketSummary {
    pub conversations: usize,
    pub chunks: usize,
    /// Max chunk count in this bucket (drives long-tail outlier
    /// awareness even though the bucket boundary already gates).
    pub max_chunks_in_conv: usize,
}

/// Run tiered enrichment over a corpus's Lance index.
///
/// v0 (step 3): reads the index, groups by `source_doc_id`, classifies
/// each conversation into a `ConvBucket`, emits stats. Returns the
/// dispatch plan for upstream observability. No LLM calls, no SQLite
/// writes.
///
/// Subsequent steps will:
///
/// - **Step 4**: invoke a `TieredEnrichmentProvider` trait (impl in
///   `sovereign-tools`) per non-`Tiny` conversation to build the T2
///   entity graph + persist into `conv_skeletons.skeleton_json`.
/// - **Step 5**: same trait extends to T3 — `build_raptor_atlas`
///   per conv, motif extraction + classification, segments via
///   TextTiling, persist into `conv_raptor_nodes` / `conv_motifs`.
pub async fn run_tiered_enrichment(
    recipe: &Recipe,
    index_path: &Path,
    provider: Option<&TieredProviderHandle>,
    entity_extractor: Option<&ChunkEntityExtractorHandle>,
) -> Result<TieredDispatchPlan> {
    let corpus_id = recipe.corpus.id.clone();
    tracing::info!(
        corpus = %corpus_id,
        index = %index_path.display(),
        has_provider = provider.is_some(),
        has_entity_extractor = entity_extractor.is_some(),
        "tiered enrichment: scanning chunks index for per-conversation grouping"
    );

    let index = CorpusIndex::open(index_path).await?;
    let groups = index.group_chunks_by_source_doc().await?;

    let mut plan = TieredDispatchPlan {
        corpus_id: corpus_id.clone(),
        total_conversations: groups.len(),
        total_chunks: groups.values().map(|v| v.len()).sum(),
        per_bucket: BTreeMap::new(),
    };

    let mut conv_buckets: Vec<(String, ConvBucket)> = Vec::with_capacity(groups.len());
    for (conv_uuid, chunk_ids) in &groups {
        let bucket = ConvBucket::classify(chunk_ids.len());
        let entry = plan.per_bucket.entry(bucket).or_default();
        entry.conversations += 1;
        entry.chunks += chunk_ids.len();
        entry.max_chunks_in_conv = entry.max_chunks_in_conv.max(chunk_ids.len());
        conv_buckets.push((conv_uuid.clone(), bucket));
    }

    tracing::info!(
        corpus = %corpus_id,
        conversations = plan.total_conversations,
        chunks = plan.total_chunks,
        "tiered enrichment: dispatch plan computed (per-bucket stats below)"
    );
    for (bucket, summary) in &plan.per_bucket {
        tracing::info!(
            corpus = %corpus_id,
            bucket = bucket.label(),
            conversations = summary.conversations,
            chunks = summary.chunks,
            max_chunks_in_conv = summary.max_chunks_in_conv,
            "tiered enrichment: bucket summary"
        );
    }

    let Some(provider) = provider else {
        tracing::warn!(
            corpus = %corpus_id,
            "tiered enrichment: no TieredEnrichmentProvider injected — emitting dispatch plan only. Wire one via CorpusEngine::with_tiered_provider to actually run T2/T3."
        );
        return Ok(plan);
    };

    // Per-conversation dispatch. Sort by ascending chunk count so the
    // cheapest (`Tiny`) work fires first — operator sees progress
    // quickly and any provider crash on a tiny conv surfaces before
    // committing to long-tail Slow-slot work.
    conv_buckets.sort_by_key(|(uuid, _)| {
        groups.get(uuid).map(|v| v.len()).unwrap_or(0)
    });

    let mut completed = 0usize;
    let mut failed = 0usize;
    for (conv_uuid, bucket) in conv_buckets {
        let rows = match index
            .chunks_for_source_doc_with_embeddings(&conv_uuid)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    conv = %conv_uuid,
                    error = %e,
                    "tiered enrichment: chunk fetch failed; skipping conv"
                );
                failed += 1;
                continue;
            }
        };
        if rows.is_empty() {
            // Group said this conv had chunks but the embedding-aware
            // fetch returned none. Likely T1 embeddings missing for
            // this conv — log and skip.
            tracing::warn!(
                corpus = %corpus_id,
                conv = %conv_uuid,
                "tiered enrichment: conv has no embedded chunks; skipping"
            );
            failed += 1;
            continue;
        }
        let (chunks, embeddings): (Vec<_>, Vec<_>) = rows.into_iter().unzip();

        // Cheap CPU-only pass first: per-chunk NER. Runs ahead of
        // the LLM-heavy provider call so even if the provider fails
        // (e.g. inference timeout), chunk_entities still
        // populates and the conv-entity-graph builder has something
        // dense to work with. Optional — None means "skip NER".
        if let Some(extractor) = entity_extractor {
            // Clone chunks because the provider call below consumes
            // the originals. Cheap relative to the NER work itself.
            let chunks_for_ner = chunks.clone();
            match extractor
                .extract_for_conversation(&corpus_id, &conv_uuid, chunks_for_ner)
                .await
            {
                Ok(n) => {
                    tracing::debug!(
                        corpus = %corpus_id,
                        conv = %conv_uuid,
                        mentions = n,
                        "tiered enrichment: per-chunk NER persisted"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        corpus = %corpus_id,
                        conv = %conv_uuid,
                        error = %e,
                        "tiered enrichment: per-chunk NER failed; continuing with RAPTOR-only entities for this conv"
                    );
                }
            }
        }

        if let Err(e) = provider
            .enrich_conversation(&corpus_id, &conv_uuid, chunks, embeddings, bucket)
            .await
        {
            tracing::warn!(
                corpus = %corpus_id,
                conv = %conv_uuid,
                bucket = bucket.label(),
                error = %e,
                "tiered enrichment: provider failed for conv; continuing with next"
            );
            failed += 1;
            continue;
        }
        completed += 1;
    }
    tracing::info!(
        corpus = %corpus_id,
        completed,
        failed,
        "tiered enrichment: per-conv dispatch finished"
    );

    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_boundaries_match_spec() {
        assert_eq!(ConvBucket::classify(0), ConvBucket::Tiny);
        assert_eq!(ConvBucket::classify(7), ConvBucket::Tiny);
        assert_eq!(ConvBucket::classify(8), ConvBucket::Small);
        assert_eq!(ConvBucket::classify(30), ConvBucket::Small);
        assert_eq!(ConvBucket::classify(31), ConvBucket::Medium);
        assert_eq!(ConvBucket::classify(100), ConvBucket::Medium);
        assert_eq!(ConvBucket::classify(101), ConvBucket::Large);
        assert_eq!(ConvBucket::classify(300), ConvBucket::Large);
        assert_eq!(ConvBucket::classify(301), ConvBucket::LongTail);
        assert_eq!(ConvBucket::classify(510), ConvBucket::LongTail);
    }

    #[test]
    fn bucket_labels_are_stable() {
        assert_eq!(ConvBucket::Tiny.label(), "tiny");
        assert_eq!(ConvBucket::Small.label(), "small");
        assert_eq!(ConvBucket::Medium.label(), "medium");
        assert_eq!(ConvBucket::Large.label(), "large");
        assert_eq!(ConvBucket::LongTail.label(), "long_tail");
    }
}
