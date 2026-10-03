// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared `#[cfg(test)]` fixtures for the bootstrap builders: a no-op
//! `InferenceProvider` stub. It lets each builder unit-test with the
//! project's standard mocks — concrete proof that the bootstrap phases are
//! CI-testable via dependency injection (only the literal model load is not).

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};

/// Minimal provider. Builders only *store* the provider (in a checker /
/// InsightService) during construction — they never call it — so the
/// completion paths are unreachable here.
pub(crate) struct StubInference;

#[async_trait::async_trait]
impl InferenceProvider for StubInference {
    async fn complete(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_contracts::error::Result<CompletionResponse> {
        unimplemented!("StubInference: not exercised by the builders under test")
    }
    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_contracts::error::Result<
        Pin<Box<dyn Stream<Item = sovereign_contracts::error::Result<String>> + Send>>,
    > {
        unimplemented!("StubInference: not exercised by the builders under test")
    }
    async fn embed(&self, _text: &str) -> sovereign_contracts::error::Result<Vec<f32>> {
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

// `temp_corpus_engine` stood here and is GONE (svt-6). Zero callers: the
// builders that took an engine went with the engine itself, and a fixture
// nothing constructs is inventory, not coverage (ARCH principle 12).
