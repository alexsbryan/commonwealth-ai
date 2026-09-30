// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one `LocalInferenceService` double for tests that assert a daemon
//! route's plumbing (phase-b pb-serve-ranks-tests-daemon). It wraps an
//! `InferenceProvider` (usually `TestProvider`) and does the least OpenAI
//! translation a route needs: the last message is the prompt, the provider's
//! text is the one choice, and stream frames are copied per variant. The real
//! translation is serve's `SovereignInferenceAdapter`, tested in its own crate;
//! a daemon test naming it would link serve for no assertion of the daemon's.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use sovereign_contracts::error::Result;
use sovereign_contracts::oicp::openai_types::{
    self as wire, ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage,
};
use sovereign_contracts::oicp::{LocalInferenceError, ProviderManifest};
use sovereign_contracts::traits::{InferenceProvider, LocalInferenceService};
use sovereign_contracts::types::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderCapabilities, Speed, StreamFrame,
};

pub struct ProviderService(pub Arc<dyn InferenceProvider>);

impl ProviderService {
    pub fn new(provider: Arc<dyn InferenceProvider>) -> Arc<dyn LocalInferenceService> {
        Arc::new(Self(provider))
    }

    fn completion_request(request: &ChatCompletionRequest) -> CompletionRequest {
        let prompt = request
            .messages
            .last()
            .map(|m| m.content.as_str())
            .unwrap_or("");
        let mut req = CompletionRequest::new(prompt);
        if let Some(model) = request.model.as_deref() {
            req = req.with_model_id(model);
        }
        req
    }
}

fn wire_reason(reason: FinishReason) -> wire::FinishReason {
    match reason {
        FinishReason::Stop => wire::FinishReason::Stop,
        FinishReason::Length => wire::FinishReason::Length,
        FinishReason::ToolCalls => wire::FinishReason::ToolCalls,
        FinishReason::ContentFilter => wire::FinishReason::ContentFilter,
        FinishReason::Cancelled => wire::FinishReason::Cancelled,
        FinishReason::Error(msg) => wire::FinishReason::Error(msg),
    }
}

fn wire_frame(frame: StreamFrame) -> wire::StreamFrame {
    match frame {
        StreamFrame::Token(text) => wire::StreamFrame::Token(text),
        StreamFrame::Finish { reason, usage } => wire::StreamFrame::Finish {
            reason: wire_reason(reason),
            usage: usage.map(|u| wire::StreamUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        },
        StreamFrame::Error(msg) => wire::StreamFrame::Error(msg),
    }
}

fn provider_error(e: sovereign_contracts::Error) -> LocalInferenceError {
    LocalInferenceError::Other(e.to_string())
}

#[async_trait]
impl InferenceProvider for ProviderService {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.0.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.0.complete_stream(request).await
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        self.0.complete_stream_with_finish(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.0.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.0.embed_batch(texts).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.0.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.0.embed_model_id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.0.capabilities()
    }
}

#[async_trait]
impl LocalInferenceService for ProviderService {
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> std::result::Result<ChatCompletionResponse, LocalInferenceError> {
        let resp = self
            .0
            .complete(&Self::completion_request(&request))
            .await
            .map_err(provider_error)?;
        let reason = resp
            .finish_reason
            .map(wire_reason)
            .unwrap_or(wire::FinishReason::Stop);
        Ok(ChatCompletionResponse {
            id: "chatcmpl-double".into(),
            object: "chat.completion".into(),
            created: 0,
            model: resp.model_id,
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: resp.text,
                    tool_call_id: None,
                    tool_calls: None,
                },
                finish_reason: Some(reason.as_openai_str().to_string()),
            }],
            usage: None,
        })
    }

    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> std::result::Result<
        Pin<Box<dyn Stream<Item = wire::StreamFrame> + Send>>,
        LocalInferenceError,
    > {
        let inner = self
            .0
            .complete_stream_with_finish(&Self::completion_request(&request))
            .await
            .map_err(provider_error)?;
        Ok(Box::pin(inner.map(wire_frame)))
    }

    fn provider_manifest(&self) -> Option<ProviderManifest> {
        None
    }
}
