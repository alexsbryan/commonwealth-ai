//! Shared `#[cfg(test)]` fixtures for the bootstrap builders: a no-op
//! `InferenceProvider` stub and a temp `CorpusEngine`. These let each
//! builder unit-test with the project's standard mocks — concrete proof
//! that the bootstrap phases are CI-testable via dependency injection
//! (only the literal model load is not).

use std::pin::Pin;
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use futures::Stream;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};
use tempfile::TempDir;

/// Minimal provider. Builders only *store* the provider (in a checker /
/// InsightService) during construction — they never call it — so the
/// completion paths are unreachable here.
pub(crate) struct StubInference;

#[async_trait::async_trait]
impl InferenceProvider for StubInference {
    async fn complete(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        unimplemented!("StubInference: not exercised by the builders under test")
    }
    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
    > {
        unimplemented!("StubInference: not exercised by the builders under test")
    }
    async fn embed(&self, _text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Slow,
            relative_reasoning: Depth::Deep,
        }
    }
}

/// A temp-dir-backed `CorpusEngine` with a zero-vector embed fn. Returns
/// the `TempDir` guard too — keep it in scope for the engine's lifetime.
pub(crate) fn temp_corpus_engine() -> (TempDir, Arc<CorpusEngine>) {
    let tmp = tempfile::tempdir().expect("corpus tempdir");
    let embed: corpus_engine::EmbedFn =
        Arc::new(|_t: &str| Box::pin(async move { Ok(vec![0.0f32; 8]) }));
    let engine = Arc::new(CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        embed,
    ));
    (tmp, engine)
}
