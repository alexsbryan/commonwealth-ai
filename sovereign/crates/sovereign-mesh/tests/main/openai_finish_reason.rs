// SPDX-License-Identifier: AGPL-3.0-or-later
//! `finish_reason` on the non-streaming chat path.
//!
//! It is how a client tells a TRUNCATED answer from a finished one.
//! The non-streaming path hardcoded `"stop"`, so a turn cut off at
//! `max_tokens` was indistinguishable from a clean finish — while the
//! streaming path, translating the same provider reason, reported
//! `"length"` correctly. Two answers for one turn
//! (ARCH_PRINCIPLES §10.6).
//!
//! Measured 2026-08-29 against the live daemon: `max_tokens: 24`
//! returned `completion_tokens: 24` with `finish_reason: "stop"`.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_api::openai_types::ChatCompletionRequest;
use commonwealth_api::state::LocalInferenceService;
use futures::Stream;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, FinishReason, ProviderCapabilities, Speed,
};
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

/// Reports exactly the finish reason it was built with, so the test
/// asserts on the ADAPTER's translation and nothing else.
struct Fixed(Option<FinishReason>);

#[async_trait]
impl InferenceProvider for Fixed {
    async fn complete(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_core::Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: "hello".into(),
            tokens_used: 24,
            prompt_tokens: 0,
            model_id: "stub".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: self.0.clone(),
            completion_tokens: Some(24),
        })
    }

    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_core::Result<Pin<Box<dyn Stream<Item = sovereign_core::Result<String>> + Send>>>
    {
        unimplemented!("not on this path")
    }

    async fn embed(&self, _text: &str) -> sovereign_core::Result<Vec<f32>> {
        unimplemented!("not on this path")
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 32_768,
            supports_structured_output: false,
            relative_speed: Speed::Slow,
            relative_reasoning: Depth::Moderate,
        }
    }
}

async fn served_reason(provider_said: Option<FinishReason>) -> String {
    let adapter = SovereignInferenceAdapter::new(Arc::new(Fixed(provider_said)));
    let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "primary",
        "messages": [{"role": "user", "content": "count to a hundred"}],
        "max_tokens": 24,
    }))
    .expect("request shape");
    adapter
        .chat_completion(request)
        .await
        .expect("stub serves")
        .choices[0]
        .finish_reason
        .clone()
        .expect("a served choice always carries a reason")
}

/// THE regression. A truncated answer must SAY it was truncated.
#[tokio::test]
async fn truncation_reaches_the_client_as_length() {
    assert_eq!(
        served_reason(Some(FinishReason::Length)).await,
        "length",
        "the engine computed Length and the adapter must not flatten it to stop — a client \
         cannot decide whether to continue a turn it believes finished cleanly"
    );
}

#[tokio::test]
async fn a_clean_stop_is_still_stop() {
    assert_eq!(served_reason(Some(FinishReason::Stop)).await, "stop");
}

/// Providers that don't track the distinction (stubs, older backends)
/// keep the historical shape rather than inventing one.
#[tokio::test]
async fn an_untracked_reason_falls_back_to_stop() {
    assert_eq!(served_reason(None).await, "stop");
}
