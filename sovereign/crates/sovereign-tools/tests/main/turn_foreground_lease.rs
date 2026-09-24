// SPDX-License-Identifier: AGPL-3.0-or-later
//! A turn holds the corpus engine's foreground lease. Moved from
//! `sovereign-core/tests/main/core_tests.rs`: it builds a real
//! `corpus_engine::CorpusEngine`, and tests that build corpus-engine live
//! beside their owner (FIVE_PROGRAMS §12 D6).

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::executor::AutoApprovalChannel;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::runtime::Runtime;
use sovereign_core::skills::SkillRegistry;
use sovereign_core::stubs::{NoOpPlanner, PassthroughRouter};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};

/// Answers every request with one fixed text, streamed as a single chunk,
/// so a turn runs to completion through the real streaming spawn.
struct OneChunk(&'static str);

#[async_trait]
impl InferenceProvider for OneChunk {
    async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: self.0.to_string(),
            tokens_used: 10,
            prompt_tokens: 0,
            model_id: "mock".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let text = self.0.to_string();
        Ok(Box::pin(futures::stream::once(async move { Ok(text) })))
    }
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("mock".to_string()))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

/// Drain a stream handle fully (the spawn persists the message before
/// the channel closes, so the store is consistent after this returns).
async fn drain(handle: sovereign_core::runtime::StreamHandle) {
    use futures::StreamExt;
    let mut stream = handle.stream;
    while stream.next().await.is_some() {}
}

/// Counts the turn-level foreground contract (issue #57 rec 4).
#[derive(Default)]
struct CountingForeground {
    begun: std::sync::atomic::AtomicUsize,
    ended: std::sync::atomic::AtomicUsize,
}
impl corpus_engine::ForegroundSignal for CountingForeground {
    fn begin(&self) {
        self.begun.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn end(&self) {
        self.ended.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A turn holds the corpus engine's foreground lease from the moment its
/// handle exists until its stream is dropped, so every background yield
/// gate reading the paired signal parks for the WHOLE turn. The failing
/// inputs this names: a lease taken per model call (the gate opens inside
/// the claim-search fan-out, measured 2026-09-02 as the newsworthy tick
/// resuming mid-turn) or never taken at all (the pre-2026-09-02 state for
/// every in-process chat path).
#[tokio::test]
async fn a_turn_holds_the_foreground_lease_until_its_stream_is_dropped() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let recipes = dir.path().join("recipes");
    let indexes = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::create_dir_all(&indexes).unwrap();
    let embed: corpus_index::types::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }));
    let engine = Arc::new(corpus_engine::CorpusEngine::new(recipes, indexes, embed));
    let signal = Arc::new(CountingForeground::default());
    let as_signal: Arc<dyn corpus_engine::ForegroundSignal> = signal.clone();
    engine.set_foreground_signal(as_signal);

    let store = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let runtime = Runtime::new(sovereign_core::RuntimeParts {
        corpus_engine: Some(engine),
        ..sovereign_core::RuntimeParts::new(
            Arc::new(OneChunk("A short answer.")),
            Box::new(PassthroughRouter),
            Box::new(NoOpPlanner),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(SkillRegistry::new()),
            Arc::new(AutoApprovalChannel),
            sovereign_core::types::InferenceConfig::default(),
            sovereign_core::runtime::lane::LaneSources::none(),
        )
    });

    let handle = runtime
        .handle_message_stream("hello there", "c1")
        .await
        .unwrap();
    assert_eq!(
        signal.begun.load(SeqCst),
        1,
        "the lease is taken with the handle"
    );
    assert_eq!(
        signal.ended.load(SeqCst),
        0,
        "and held while the stream is live"
    );
    drain(handle).await;
    assert_eq!(
        signal.ended.load(SeqCst),
        1,
        "released only when the stream is dropped"
    );

    // A second turn takes its own lease; nothing is remembered between turns.
    let handle = runtime
        .handle_message_stream("and again", "c1")
        .await
        .unwrap();
    assert_eq!(signal.begun.load(SeqCst), 2);
    drop(handle);
    assert_eq!(
        signal.ended.load(SeqCst),
        2,
        "a client that goes away releases it too"
    );
}
