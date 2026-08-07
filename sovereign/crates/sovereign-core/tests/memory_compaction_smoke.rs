// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end smoke for the rolling-summary memory compaction
//! worker. Exercises the structural answer to the witness-prompt
//! growth bug observed 2026-05-23 — see
//! [[witness-memory-rolling-compaction]] plan.
//!
//! The bench fixture at `sovereign/bench/inner_work/compaction.toml`
//! is the *behavioural* verification (does the witness still respond
//! after 12 turns; does turn-1-5 quality stay within noise). That
//! bench needs the daemon's loaded fast slot, so it runs out-of-test.
//!
//! This file is the *mechanical* verification: the worker actually
//! folds, retrieval actually filters superseded rows, the scope wall
//! is actually preserved on the new summary. If any of these break,
//! the bench can't pass either way.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::memory_compaction::{CompactionConfig, CompactionMode, CompactionWorker};
use sovereign_core::traits::{InferenceProvider, MemoryStore};
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, Memory, MemoryKind, ProviderCapabilities, Speed,
};
use sovereign_store::memory::InMemoryStateStore;

/// Returns a fixed synthesized summary plus records the prompt for
/// the assertion that wants to confirm the entries block was wired
/// through correctly.
struct StubInference {
    response: String,
    last_prompt: Mutex<Option<String>>,
}

impl StubInference {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            last_prompt: Mutex::new(None),
        }
    }
}

#[async_trait]
impl InferenceProvider for StubInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        *self.last_prompt.lock().unwrap() = Some(request.prompt.clone());
        Ok(CompletionResponse {
            text: self.response.clone(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "stub".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
            ..Default::default()
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "StubInference: streaming unused".into(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

fn raw_mem(id: &str, conv: &str, created_at: i64, content: &str) -> Memory {
    Memory {
        id: id.into(),
        content: content.into(),
        source: "test".into(),
        confidence: 0.9,
        created_at,
        last_used: created_at,
        source_conversation_id: Some(conv.into()),
        source_skill_id: Some("inner-work".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn worker_folds_oldest_batch_when_threshold_crossed() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryStateStore::new());
    let inference: Arc<dyn InferenceProvider> =
        Arc::new(StubInference::new("Synthesised summary of three entries."));
    let cfg = CompactionConfig {
        threshold: 6,
        batch: 3,
        mode: CompactionMode::Sync,
        max_summary_chars: 200,
        synthesis_prompt: "Distill:\n{entries}".into(),
    };
    let worker = CompactionWorker::spawn(Arc::clone(&store), Arc::clone(&inference), cfg);

    let conv = "conv-A";
    for i in 0..6 {
        store
            .save_memory(&raw_mem(
                &format!("m{i}"),
                conv,
                1_700_000_000 + i,
                &format!("entry {i} body"),
            ))
            .await
            .unwrap();
    }

    let pass = worker
        .run_one_sync(conv)
        .await
        .unwrap()
        .expect("threshold crossed; expected one pass");
    assert_eq!(pass.source_memory_ids, vec!["m0", "m1", "m2"]);
    assert!(pass.summary_id.is_some());

    let active = store.list_memories_for_conversation(conv).await.unwrap();
    assert_eq!(active.len(), 4, "1 summary + 3 untouched raws");
    let summary = active
        .iter()
        .find(|m| matches!(m.kind, MemoryKind::Summary))
        .expect("summary present");
    assert_eq!(summary.source_memory_ids, vec!["m0", "m1", "m2"]);
    assert_eq!(
        summary.source_skill_id.as_deref(),
        Some("inner-work"),
        "summary inherits scope from sources — privacy wall is structural",
    );
    assert!(summary.content.starts_with("Synthesised summary"));

    let all = store.get_all_memories().await.unwrap();
    assert_eq!(
        all.len(),
        4,
        "get_all_memories must filter superseded — 6 raws - 3 superseded + 1 summary = 4"
    );
}

#[tokio::test]
async fn worker_under_threshold_is_noop() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryStateStore::new());
    let inference: Arc<dyn InferenceProvider> =
        Arc::new(StubInference::new("would never be called"));
    let cfg = CompactionConfig {
        threshold: 6,
        batch: 3,
        mode: CompactionMode::Sync,
        ..Default::default()
    };
    let worker = CompactionWorker::spawn(store.clone(), inference, cfg);

    let conv = "conv-B";
    for i in 0..5 {
        store
            .save_memory(&raw_mem(
                &format!("n{i}"),
                conv,
                1_700_000_000 + i,
                "short entry",
            ))
            .await
            .unwrap();
    }
    let pass = worker.run_one_sync(conv).await.unwrap();
    assert!(
        pass.is_none(),
        "5 memories is under threshold=6; should no-op"
    );

    let active = store.list_memories_for_conversation(conv).await.unwrap();
    assert_eq!(active.len(), 5);
    assert!(active.iter().all(|m| matches!(m.kind, MemoryKind::Raw)));
}

#[tokio::test]
async fn disabled_mode_skips_compaction_even_above_threshold() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryStateStore::new());
    let inference: Arc<dyn InferenceProvider> = Arc::new(StubInference::new("never used"));
    let cfg = CompactionConfig {
        threshold: 6,
        batch: 3,
        mode: CompactionMode::Disabled,
        ..Default::default()
    };
    let worker = CompactionWorker::spawn(store.clone(), inference, cfg);

    let conv = "conv-C";
    for i in 0..10 {
        store
            .save_memory(&raw_mem(&format!("p{i}"), conv, 1_700_000_000 + i, "entry"))
            .await
            .unwrap();
    }
    // maybe_enqueue is a no-op when mode=Disabled — verify by
    // confirming retrieval still sees all 10 raws untouched.
    worker.maybe_enqueue(conv);
    let active = store.list_memories_for_conversation(conv).await.unwrap();
    assert_eq!(active.len(), 10);
    assert!(active.iter().all(|m| matches!(m.kind, MemoryKind::Raw)));
}

#[tokio::test]
async fn summary_truncates_at_max_summary_chars() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryStateStore::new());
    let long = "x".repeat(2000);
    let inference: Arc<dyn InferenceProvider> = Arc::new(StubInference::new(&long));
    let cfg = CompactionConfig {
        threshold: 3,
        batch: 3,
        mode: CompactionMode::Sync,
        max_summary_chars: 100,
        synthesis_prompt: "x".into(),
    };
    let worker = CompactionWorker::spawn(store.clone(), inference, cfg);

    let conv = "conv-D";
    for i in 0..3 {
        store
            .save_memory(&raw_mem(&format!("q{i}"), conv, 1_700_000_000 + i, "x"))
            .await
            .unwrap();
    }
    let pass = worker
        .run_one_sync(conv)
        .await
        .unwrap()
        .expect("threshold crossed");
    let summary_id = pass.summary_id.unwrap();
    let active = store.list_memories_for_conversation(conv).await.unwrap();
    let summary = active
        .iter()
        .find(|m| m.id == summary_id)
        .expect("summary present");
    assert_eq!(summary.content.chars().count(), 100);
    assert!(summary.content.ends_with('…'));
}
