// SPDX-License-Identifier: AGPL-3.0-or-later
//! The model-free provider, moved out of `child_main` (phase-b
//! pb-serve-program) so the compute child's `--role mock` and `serve`'s
//! `mock` engine are one mock.

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use std::sync::Arc;

use sovereign_contracts::engine_config::EngineSection;
use sovereign_contracts::traits::ResidentSlot;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, Depth, FinishReason, InferenceProvider,
    ProviderCapabilities, Result, Speed,
};
use sovereign_inference::engine_factory::{BuiltEngine, EngineBuilder};

/// The id the mock answers as, in every slot.
pub const MOCK_MODEL: &str = "mock";

/// The engine id [`MockEngine`] registers under: `[engine] kind = "mock"`.
pub const MOCK_ENGINE: &str = "mock";

/// A model-free engine, for a host that registers it
/// (`engine_factory::register_engine(MOCK_ENGINE, ..)`): a smoke that proves
/// the host's routes answer without loading weights. Never a fallback — a
/// host serves it only when its config names it.
pub struct MockEngine;

impl EngineBuilder for MockEngine {
    fn build(&self, _section: &EngineSection) -> std::result::Result<BuiltEngine, String> {
        tracing::info!(target: "engine_factory", engine = MOCK_ENGINE, "building the model-free mock engine");
        Ok(BuiltEngine::external(Arc::new(MockProvider {
            tokens: 8,
            delay: Duration::ZERO,
        })))
    }
}

/// Model-free provider: streams `tokens` canned tokens with `delay` between
/// them (so a crash-isolation test can `kill -9` mid-stream), and answers
/// `complete`/`embed` with fixed values.
pub struct MockProvider {
    /// Tokens each stream yields.
    pub tokens: usize,
    /// Delay before each streamed token.
    pub delay: Duration,
}

#[async_trait]
impl InferenceProvider for MockProvider {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: "mock response".to_string(),
            tokens_used: self.tokens,
            prompt_tokens: 0,
            model_id: "mock".to_string(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: Some(FinishReason::Stop),
            completion_tokens: Some(self.tokens as u32),
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let n = self.tokens;
        let delay = self.delay;
        let s = futures::stream::unfold(0usize, move |i| async move {
            if i >= n {
                return None;
            }
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }
            Some((Ok(format!("tok{i} ")), i + 1))
        });
        Ok(Box::pin(s))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }

    /// 1.0 for a document that contains the query, else 0.0: model-free, and
    /// enough to tell the kind route answered.
    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        Ok(docs
            .iter()
            .map(|d| if d.contains(query) { 1.0 } else { 0.0 })
            .collect())
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        MOCK_MODEL.to_string()
    }

    /// One resident slot, so a host's self-manifest advertises the mock as
    /// the weights it holds instead of reading as a node that holds none.
    fn resident_slots(&self) -> Vec<ResidentSlot> {
        vec![ResidentSlot {
            role: "primary".to_string(),
            model_id: MOCK_MODEL.to_string(),
            resident: true,
            size_bytes: None,
            transitioning: false,
            placement: None,
        }]
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
