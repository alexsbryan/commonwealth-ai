// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GlinerChunkExtractor::extract_for_conversation` — driven end to end
//! against a recording inference seam and a no-op chunk-entity port.
//!
//! **Why not a unit test on the bound.** `bounded_input.rs` already tests
//! `BoundedInputs` directly. What that cannot show is whether the
//! production entry point USES it: the failing input reaches GLiNER
//! through `extract_for_conversation`, and before 2026-09-12 that method
//! built one `Vec<&str>` of every chunk in the conversation and made a
//! single `extract_mentions_batch` call. A bound nothing calls is not a
//! bound (ARCH 5).
//!
//! **The failing input**, from the work order: one conversation of 40
//! chunks × 60,000 characters. Watched red against the unbounded method,
//! green after.
//!
//! The real incident was the other axis — `threaded_turns` caps a chunk at
//! 2,100 chars, so the pass that took the daemon 20.2 GB → 79.9 GB in
//! eight minutes (pid 47944, 2026-09-12) was thousands of SHORT chunks in
//! one `inference()` call. `a_thousand_short_chunks_reach_the_seam_in_bounded_batches`
//! is that case.

use std::sync::{Arc, Mutex};

use corpus_engine::enrichment::tiered::ChunkEntityExtractor;
use corpus_index::index::EnrichmentChunkRow;
use sovereign_contracts::daemon_wire::conv_tiered::{
    ChunkEntityProgressRow, ChunkEntityRow, ChunkEntityStore,
};
use sovereign_contracts::error::Result;
use sovereign_gliner::bounded_input::{MAX_BATCH_CHUNKS, MAX_CHUNK_CHARS};
use sovereign_gliner::gliner_ner::EntityMention;
use sovereign_gliner::{GlinerChunkExtractor, LabeledEntityExtractor};

/// Stands in for GLiNER and records the SHAPE of every call — which is
/// the whole assertion: what the model is handed, not what the caller
/// meant to hand it.
#[derive(Default)]
struct RecordingSeam {
    /// One entry per `extract_mentions_batch` call: byte length of each
    /// text in that batch.
    batches: Mutex<Vec<Vec<usize>>>,
}

impl LabeledEntityExtractor for RecordingSeam {
    fn model_id(&self) -> &str {
        "gliner_small-v2.1"
    }
    fn labels(&self) -> Vec<String> {
        vec!["Person".into()]
    }
    fn threshold(&self) -> f32 {
        0.6
    }
    fn extract_mentions(&self, _text: &str) -> Result<Vec<EntityMention>> {
        Ok(Vec::new())
    }
    fn extract_mentions_batch(&self, texts: &[&str]) -> Result<Vec<Vec<EntityMention>>> {
        self.batches
            .lock()
            .unwrap()
            .push(texts.iter().map(|t| t.len()).collect());
        Ok(vec![Vec::new(); texts.len()])
    }
}

fn row(id: u64, content: String) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content,
        title: Some("conv-a".into()),
        url: None,
        metadata_raw: None,
        source_doc_id: Some("conv-a".into()),
    }
}

/// Stands in for the state store: `extract_for_conversation` only writes
/// through `save_chunk_entities_for_conv`, and these tests assert on the
/// inference seam's shape, never on persistence.
struct NoopStore;

#[async_trait::async_trait]
impl ChunkEntityStore for NoopStore {
    async fn list_ner_processed_chunk_ids(
        &self,
        _corpus_id: &str,
    ) -> Result<std::collections::HashSet<u64>> {
        Ok(std::collections::HashSet::new())
    }
    async fn save_chunk_entities(&self, _rows: &[ChunkEntityRow]) -> Result<()> {
        Ok(())
    }
    async fn record_ner_processed_chunks(
        &self,
        _corpus_id: &str,
        _chunk_ids: &[u64],
    ) -> Result<()> {
        Ok(())
    }
    async fn get_chunk_entity_progress(
        &self,
        _corpus_id: &str,
    ) -> Result<Option<ChunkEntityProgressRow>> {
        Ok(None)
    }
    async fn upsert_chunk_entity_progress(&self, _row: &ChunkEntityProgressRow) -> Result<()> {
        Ok(())
    }
    async fn save_chunk_entities_for_conv(
        &self,
        _corpus_id: &str,
        _conv_uuid: &str,
        _rows: &[ChunkEntityRow],
    ) -> Result<()> {
        Ok(())
    }
}

fn store() -> Arc<dyn ChunkEntityStore> {
    Arc::new(NoopStore)
}

/// Assert the seam never saw an oversized batch or an oversized text.
/// Shared so a future caller added to this path inherits the same check.
fn assert_bounded(seam: &RecordingSeam) {
    for batch in seam.batches.lock().unwrap().iter() {
        assert!(
            batch.len() <= MAX_BATCH_CHUNKS,
            "the inference seam was handed a batch of {} chunks (cap {MAX_BATCH_CHUNKS})",
            batch.len()
        );
        for len in batch {
            assert!(
                *len <= MAX_CHUNK_CHARS,
                "the inference seam was handed a {len}-byte text (cap {MAX_CHUNK_CHARS})"
            );
        }
    }
}

/// The order's named failing input.
#[tokio::test]
async fn a_forty_by_sixty_thousand_conversation_reaches_the_seam_bounded() {
    let seam = Arc::new(RecordingSeam::default());
    let extractor = GlinerChunkExtractor::new(store(), seam.clone());

    let big = "word ".repeat(12_000);
    assert_eq!(big.len(), 60_000);
    let chunks: Vec<EnrichmentChunkRow> = (0..40).map(|i| row(i, big.clone())).collect();

    let outcome = extractor
        .extract_for_conversation("agent-sessions", "conv-a", chunks)
        .await
        .expect("extraction");

    assert_bounded(&seam);
    assert_eq!(
        outcome.refused_over_cap, 40,
        "every over-cap chunk is REFUSED and counted — never truncated, never silent"
    );
    assert_eq!(outcome.mentions, 0);
}

/// The axis the incident actually took: many short chunks, one call.
#[tokio::test]
async fn a_thousand_short_chunks_reach_the_seam_in_bounded_batches() {
    let seam = Arc::new(RecordingSeam::default());
    let extractor = GlinerChunkExtractor::new(store(), seam.clone());

    // 1,500 bytes each — `threaded_turns`'s soft target, so this is what a
    // real Claude Code transcript conversation looks like.
    let chunks: Vec<EnrichmentChunkRow> = (0..1_000)
        .map(|i| row(i, format!("{i} ").repeat(300)))
        .collect();
    assert!(chunks[0].content.len() <= MAX_CHUNK_CHARS);

    let outcome = extractor
        .extract_for_conversation("agent-sessions", "conv-a", chunks)
        .await
        .expect("extraction");

    assert_bounded(&seam);
    assert_eq!(
        outcome.refused_over_cap, 0,
        "chunks the shipped chunker emits must not be refused"
    );
    let seen = seam.batches.lock().unwrap();
    assert_eq!(
        seen.iter().map(Vec::len).sum::<usize>(),
        1_000,
        "bounding must not drop a chunk"
    );
    assert_eq!(seen.len(), 1_000_usize.div_ceil(MAX_BATCH_CHUNKS));
}

/// The bound must not become a reason nothing is enriched: an ordinary
/// short conversation still reaches the seam in one call, unchanged.
#[tokio::test]
async fn a_small_conversation_is_still_one_call() {
    let seam = Arc::new(RecordingSeam::default());
    let extractor = GlinerChunkExtractor::new(store(), seam.clone());

    let chunks: Vec<EnrichmentChunkRow> = (0..3)
        .map(|i| row(i, format!("Ailsa asked about the ferry ({i})")))
        .collect();

    let outcome = extractor
        .extract_for_conversation("threads", "conv-a", chunks)
        .await
        .expect("extraction");

    assert_eq!(outcome.refused_over_cap, 0);
    let seen = seam.batches.lock().unwrap();
    assert_eq!(seen.len(), 1, "no needless extra inference calls");
    assert_eq!(seen[0].len(), 3);
}
