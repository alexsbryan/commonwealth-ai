// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `ChunkEntityExtractor` seam, driven through the real dispatch loop.
//!
//! corpus-engine declares the trait and fires it; `sovereign-tools` owns the
//! only concrete impl (where the `gline-rs` dep lives). That inversion is the
//! point of the seam — an alternative span-typed extractor plugs in behind it
//! and inherits one persistence, dedup and provenance path. Nothing exercised
//! it: every test in `tiered.rs` covers bucket classification or the terminal
//! state stamp, and the extractor is `Option<&Handle>`, so a dispatch-loop
//! change that stopped calling it would leave the corpus enriched from
//! RAPTOR-derived entities alone — fewer, coarser entities, no error, and a
//! `chunk_entities` table that is merely thinner than it should be.
//!
//! An integration test rather than a unit one because the contract is about
//! the LOOP: once per conversation group, with that group's own chunks, and
//! BEFORE the heavy provider call so a provider failure still leaves entities
//! behind. None of that is observable from a mock alone.

use std::path::Path;
use std::sync::{Arc, Mutex};

use corpus_engine::enrichment::tiered::{
    run_tiered_enrichment, ChunkEntityExtractor, ChunkEntityExtractorHandle, ConvBucket,
    TieredEnrichmentProvider, TieredProviderHandle,
};
use corpus_engine::index::{EnrichmentChunkRow, InsertChunk, InsertCodeMeta};
use corpus_engine::recipe::Recipe;
use corpus_engine::{CorpusIndex, Result};

const EMBED_DIM: usize = 8;
const RECIPE: &str = r#"
[corpus]
id = "threads"
name = "threads"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
"#;

fn embedding(seed: f32) -> Vec<f32> {
    (0..EMBED_DIM).map(|i| seed + i as f32 * 0.1).collect()
}

fn chunk(content: &str, conv: &str) -> InsertChunk {
    InsertChunk {
        content: content.into(),
        title: Some(conv.into()),
        url: None,
        metadata: None,
        content_hash: None,
        source_doc_id: Some(conv.to_string()),
        source_file: None,
        code: InsertCodeMeta::default(),
        unit_id: None,
    }
}

/// `(corpus_id, conv_uuid, chunk contents)` — one row per extractor call.
type NerCall = (String, String, Vec<String>);

#[derive(Default)]
struct RecordingExtractor {
    calls: Mutex<Vec<NerCall>>,
}

#[async_trait::async_trait]
impl ChunkEntityExtractor for RecordingExtractor {
    async fn extract_for_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
    ) -> Result<usize> {
        self.calls.lock().unwrap().push((
            corpus_id.to_string(),
            conv_uuid.to_string(),
            chunks.iter().map(|c| c.content.clone()).collect(),
        ));
        Ok(chunks.len() * 2)
    }
}

/// Records what it was asked to enrich, and can fail on demand — the
/// "inference timed out" shape the NER-first ordering exists for.
struct RecordingProvider {
    seen: Mutex<Vec<String>>,
    fail: bool,
}

#[async_trait::async_trait]
impl TieredEnrichmentProvider for RecordingProvider {
    async fn enrich_conversation(
        &self,
        _corpus_id: &str,
        conv_uuid: &str,
        _chunks: Vec<EnrichmentChunkRow>,
        _embeddings: Vec<Vec<f32>>,
        _bucket: ConvBucket,
    ) -> Result<()> {
        self.seen.lock().unwrap().push(conv_uuid.to_string());
        if self.fail {
            return Err(corpus_engine::Error::InvalidInput(
                "provider is down".into(),
            ));
        }
        Ok(())
    }
}

async fn build_index(path: &Path, rows: &[(&str, &str)]) {
    let index = CorpusIndex::create(
        path,
        "threads",
        "Threads",
        "test-model",
        EMBED_DIM,
        true,
        "MIT",
    )
    .await
    .expect("create index");
    let payload: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(i, (content, conv))| (chunk(content, conv), embedding(i as f32)))
        .collect();
    index.insert_batch(&payload).await.expect("insert_batch");
}

/// covers: EN-5
#[tokio::test]
async fn the_injected_entity_extractor_is_called_once_per_conversation_with_its_own_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("index");
    build_index(
        &index_path,
        &[
            ("Ailsa asked about the ferry", "conv-a"),
            ("and Rhona answered", "conv-a"),
            ("Separate thread about the pier", "conv-b"),
        ],
    )
    .await;

    let extractor = Arc::new(RecordingExtractor::default());
    let provider = Arc::new(RecordingProvider {
        seen: Mutex::new(Vec::new()),
        fail: false,
    });
    let provider_handle: TieredProviderHandle = provider.clone();
    let extractor_handle: ChunkEntityExtractorHandle = extractor.clone();

    let recipe = Recipe::from_toml(RECIPE).unwrap();
    let plan = run_tiered_enrichment(
        &recipe,
        &index_path,
        Some(&provider_handle),
        Some(&extractor_handle),
    )
    .await
    .expect("dispatch");

    assert_eq!(plan.total_conversations, 2);
    assert_eq!(plan.total_chunks, 3);

    let mut calls = extractor.calls.lock().unwrap().clone();
    calls.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        calls.len(),
        2,
        "exactly one extractor call per conversation group — not per chunk, not once per corpus"
    );

    // The corpus id and conv uuid it is handed are the keys `chunk_entities`
    // is written under; a wrong conv_uuid here files a conversation's entities
    // under another conversation and retrieval silently misses them.
    assert_eq!(calls[0].0, "threads");
    assert_eq!(calls[0].1, "conv-a");
    assert_eq!(
        calls[0].2,
        vec![
            "Ailsa asked about the ferry".to_string(),
            "and Rhona answered".to_string()
        ],
        "each call carries that conversation's own chunks"
    );
    assert_eq!(calls[1].1, "conv-b");
    assert_eq!(calls[1].2, vec!["Separate thread about the pier".to_string()]);

    // And the heavy provider still ran for both — the NER pass is an addition
    // to the dispatch, not a replacement for it.
    let mut seen = provider.seen.lock().unwrap().clone();
    seen.sort();
    assert_eq!(seen, vec!["conv-a".to_string(), "conv-b".to_string()]);
}

/// covers: EN-5
#[tokio::test]
async fn ner_runs_before_the_provider_so_a_provider_failure_still_leaves_entities() {
    // The documented reason the cheap CPU pass is ordered first: "even if the
    // provider fails (e.g. inference timeout), chunk_entities still populates".
    // Ordering is invisible on the happy path, so it is asserted on the
    // failing one.
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("index");
    build_index(&index_path, &[("only chunk", "conv-a")]).await;

    let extractor = Arc::new(RecordingExtractor::default());
    let provider = Arc::new(RecordingProvider {
        seen: Mutex::new(Vec::new()),
        fail: true,
    });
    let provider_handle: TieredProviderHandle = provider.clone();
    let extractor_handle: ChunkEntityExtractorHandle = extractor.clone();

    let recipe = Recipe::from_toml(RECIPE).unwrap();
    run_tiered_enrichment(
        &recipe,
        &index_path,
        Some(&provider_handle),
        Some(&extractor_handle),
    )
    .await
    .expect("a provider failure is per-conversation, not fatal to the run");

    assert_eq!(
        extractor.calls.lock().unwrap().len(),
        1,
        "the NER pass must have already run when the provider failed"
    );
    assert_eq!(provider.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn no_extractor_injected_is_a_clean_skip_not_a_failure() {
    // The `None` arm is the pre-seam behaviour and every non-conversation
    // corpus takes it. It must dispatch the provider exactly as before.
    let dir = tempfile::tempdir().unwrap();
    let index_path = dir.path().join("index");
    build_index(&index_path, &[("only chunk", "conv-a")]).await;

    let provider = Arc::new(RecordingProvider {
        seen: Mutex::new(Vec::new()),
        fail: false,
    });
    let provider_handle: TieredProviderHandle = provider.clone();

    let recipe = Recipe::from_toml(RECIPE).unwrap();
    run_tiered_enrichment(&recipe, &index_path, Some(&provider_handle), None)
        .await
        .expect("dispatch");

    assert_eq!(provider.seen.lock().unwrap().len(), 1);
}
