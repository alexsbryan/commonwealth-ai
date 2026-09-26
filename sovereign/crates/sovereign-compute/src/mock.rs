// SPDX-License-Identifier: AGPL-3.0-or-later
//! The model-free provider, moved out of `child_main` (phase-b
//! pb-serve-program) so the compute child's `--role mock` and `serve`'s
//! `mock` engine are one mock.

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, Depth, FinishReason, InferenceProvider,
    ProviderCapabilities, Result, Speed,
};

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

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}
