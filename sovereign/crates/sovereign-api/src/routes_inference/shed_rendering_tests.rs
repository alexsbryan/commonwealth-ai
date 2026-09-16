// SPDX-License-Identifier: AGPL-3.0-or-later
//! A queue shed must reach the client as backpressure, not as a
//! crash.
//!
//! These exist because the 2026-08-07 live-fleet probe caught the
//! opposite: a caller whose peer had declined landed on a busy local
//! slot and got `{"type":"backend_error"}` with its retry hint buried
//! in prose and no `Retry-After` header. Note `bef03728` had recorded
//! the gap; nothing failed until the probe supplied the input.

use super::*;
use crate::state::{test_app_state, LocalInferenceError, LocalInferenceService};
use axum::http::header::RETRY_AFTER;
use futures::Stream;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
use std::pin::Pin;
use std::sync::Arc;

/// The exact condition the probe hit: queue position 6, ~34.7 s
/// predicted wait, past the 30 s bound.
struct AlwaysSheds;

impl AlwaysSheds {
    fn shed() -> LocalInferenceError {
        LocalInferenceError::Shed {
            position: 6,
            predicted_wait_ms: 34_746,
            retry_after_secs: 35,
        }
    }
}

#[async_trait::async_trait]
impl InferenceProvider for AlwaysSheds {
    async fn complete(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        unimplemented!("chat not used on the shed path")
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
    > {
        unimplemented!("chat not used on the shed path")
    }
    async fn embed(&self, _i: &str) -> sovereign_core::error::Result<Vec<f32>> {
        unimplemented!("embedding is not on the shed path")
    }
    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!()
    }
}

#[async_trait::async_trait]
impl LocalInferenceService for AlwaysSheds {
    async fn chat_completion(
        &self,
        _r: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LocalInferenceError> {
        Err(Self::shed())
    }
    async fn chat_completion_stream(
        &self,
        _r: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, LocalInferenceError> {
        Err(Self::shed())
    }
    fn provider_manifest(&self) -> Option<oicp_types::ProviderManifest> {
        None
    }
}

fn chat_request() -> ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": "primary",
        "messages": [{ "role": "user", "content": "hi" }],
    }))
    .expect("test request builds")
}

async fn assert_reads_as_backpressure(resp: Response, lane: &str) {
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "{lane}: a shed is a 503"
    );
    // The load-bearing assertion. Without `Retry-After` a client
    // cannot distinguish "busy, come back in 35s" from "this broke",
    // which is precisely what the probe observed.
    assert_eq!(
        resp.headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("35"),
        "{lane}: a queue shed MUST carry Retry-After"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("body is json");
    assert_eq!(
        json["reason"], "local_queue_full",
        "{lane}: the reason is structured, not prose-only"
    );
    assert_eq!(json["retry_after_secs"], 35, "{lane}: retry survives typed");
    assert_ne!(
        json["type"], "backend_error",
        "{lane}: backpressure must not be typed as a backend failure"
    );
    // This route is advertised as OpenAI-compatible, so the message
    // has to arrive where an OpenAI client looks for it. Serialising
    // `error` as a bare string meant the one thing a shed needs to
    // say ("busy, come back in 35s") was the one thing a
    // third-party SDK could not read.
    let message = json["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("host busy"),
        "{lane}: the cause belongs at error.message, got {}",
        json["error"]
    );
    assert_eq!(
        json["error"]["type"], "server_error",
        "{lane}: OpenAI `type` is the coarse bucket"
    );
    assert_eq!(
        json["error"]["code"], "local_queue_full",
        "{lane}: OpenAI `code` carries the precise reason, mirroring `reason`"
    );
}

#[tokio::test]
async fn non_streaming_shed_reads_as_backpressure() {
    let state = test_app_state().with_local_inference(Arc::new(AlwaysSheds));
    let resp = serve_local_non_stream(
        Arc::new(AlwaysSheds),
        chat_request(),
        state,
        None,
        "primary".to_string(),
    )
    .await;
    assert_reads_as_backpressure(resp, "non-streaming").await;
}

#[tokio::test]
async fn streaming_shed_reads_as_backpressure() {
    // The lane most clients actually take.
    let state = test_app_state().with_local_inference(Arc::new(AlwaysSheds));
    let resp = serve_local_stream(
        Arc::new(AlwaysSheds),
        chat_request(),
        state,
        None,
        "primary".to_string(),
    )
    .await;
    assert_reads_as_backpressure(resp, "streaming").await;
}
