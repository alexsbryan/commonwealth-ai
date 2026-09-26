// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/completions` — the FIM inline-completion route
//! (`sovereign/docs/INLINE_COMPLETION.md` §3.4, decision D6).
//!
//! Deliberately thin: parse the dual wire shape (OpenAI-legacy
//! `prompt`+`suffix` vs the rich `prefix`+`suffix` the first-party
//! extension sends), unify onto [`FimCompletionRequest`], delegate to
//! [`LocalInferenceService::fim_completion_stream`], and bridge the
//! frame stream to either an aggregated OpenAI `text_completion`
//! object or SSE chunks + `[DONE]`. All prompt assembly, slot
//! routing, and stop-craft lives behind the seam (sovereign-mesh's
//! `fim_adapter`), so this handler never learns model details.
//!
//! Failure contract: 503 with an actionable body whenever the seam
//! errors — the adapter's message carries the exact `[models.fim]`
//! fix, and we surface it verbatim (a friend setting this up should
//! never have to read daemon logs for the common misconfigurations).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sovereign_serving_host::fim_http::{serve_fim_aggregated, serve_fim_sse};

use crate::openai_types::{CompletionsRequestWire, ErrorResponse, StopParam};
use crate::state::{AppState, FimCompletionRequest};

/// POST /v1/completions.
pub async fn completions(
    State(state): State<AppState>,
    Json(wire): Json<CompletionsRequestWire>,
) -> Response {
    // Foreground-yield bump: same rationale as /v1/chat/completions —
    // keystroke-path latency must preempt background ingest work.
    state.bump_foreground_active();

    let Some(prefix) = wire.effective_prefix().map(str::to_string) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ErrorResponse::new(
                    "missing `prefix` (or legacy `prompt`): /v1/completions is the FIM \
                     inline-completion surface and needs the code before the cursor",
                    "invalid_request",
                ))
                .unwrap_or_default(),
            ),
        )
            .into_response();
    };

    let Some(service) = state.inner.serving.local_inference.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ErrorResponse::new(
                    "no local inference service on this node — FIM completions need the \
                     embedded llama.cpp service (sovereign daemon)",
                    "model_not_ready",
                ))
                .unwrap_or_default(),
            ),
        )
            .into_response();
    };

    let debug_wanted = wire.debug.unwrap_or(false);
    let model_echo = wire.model.clone();
    let want_stream = wire.stream.unwrap_or(false);
    let request = FimCompletionRequest {
        prefix,
        suffix: wire.suffix.clone().unwrap_or_default(),
        path: wire.path.clone(),
        language: wire.language.clone(),
        max_tokens: wire.max_tokens,
        temperature: wire.temperature,
        stop: wire
            .stop
            .clone()
            .map(StopParam::into_vec)
            .unwrap_or_default(),
        debug: debug_wanted,
        raw_prompt: None,
    };

    let start = match service.fim_completion_stream(request).await {
        Ok(s) => s,
        Err(e) => {
            // The adapter's message is the operator-facing fix
            // (unconfigured → exact [models.fim] snippet; marker-less
            // model → which GGUF shape to use). Surface verbatim.
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::to_value(ErrorResponse::new(e, "fim_unavailable"))
                        .unwrap_or_default(),
                ),
            )
                .into_response();
        }
    };

    if want_stream {
        serve_fim_sse(start, debug_wanted, model_echo)
    } else {
        serve_fim_aggregated(start, debug_wanted, model_echo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::{FinishReason, StreamFrame, StreamUsage};
    use crate::state::{
        test_app_state, test_app_state_with_inference, EditSlotStatus, FimStreamStart,
        LocalInferenceService,
    };
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use futures::Stream;
    use sovereign_core::traits::InferenceProvider;
    use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Canned FIM backend: records the request it received, replays a
    /// fixed frame sequence.
    struct StubFim {
        frames: Vec<StreamFrame>,
        seen: Arc<Mutex<Option<FimCompletionRequest>>>,
    }

    #[async_trait]
    impl InferenceProvider for StubFim {
        async fn complete(
            &self,
            _r: &CompletionRequest,
        ) -> sovereign_core::error::Result<CompletionResponse> {
            unimplemented!("chat not used in these tests")
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> sovereign_core::error::Result<
            Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
        > {
            unimplemented!("chat not used in these tests")
        }
        async fn embed(&self, _i: &str) -> sovereign_core::error::Result<Vec<f32>> {
            unimplemented!()
        }
        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
    }

    #[async_trait]
    impl LocalInferenceService for StubFim {
        async fn chat_completion(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<crate::openai_types::ChatCompletionResponse, crate::state::LocalInferenceError>
        {
            unimplemented!("chat not used in these tests")
        }
        async fn chat_completion_stream(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = StreamFrame> + Send>>,
            crate::state::LocalInferenceError,
        > {
            unimplemented!("chat not used in these tests")
        }
        fn provider_manifest(&self) -> Option<oicp_types::ProviderManifest> {
            None
        }
        async fn fim_completion_stream(
            &self,
            request: FimCompletionRequest,
        ) -> Result<FimStreamStart, String> {
            *self.seen.lock().unwrap() = Some(request);
            let frames = self.frames.clone();
            Ok(FimStreamStart {
                stream: Box::pin(futures::stream::iter(frames)),
                model_id: "qwen-coder-1.5b".into(),
                slot: "fim".into(),
                fim_style: "qwen_coder".into(),
            })
        }
        fn edit_status(&self) -> Option<EditSlotStatus> {
            Some(EditSlotStatus {
                slot: "edit".into(),
                model_id: "qwen-coder-1.5b".into(),
                aliased_to_fast: false,
                degraded: false,
                next_edit_format: Some("region_instruct".into()),
                fim_style: Some("qwen_coder".into()),
                advice: None,
            })
        }
    }

    struct NoFim;
    #[async_trait]
    impl InferenceProvider for NoFim {
        async fn complete(
            &self,
            _r: &CompletionRequest,
        ) -> sovereign_core::error::Result<CompletionResponse> {
            unimplemented!()
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> sovereign_core::error::Result<
            Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
        > {
            unimplemented!()
        }
        async fn embed(&self, _i: &str) -> sovereign_core::error::Result<Vec<f32>> {
            unimplemented!()
        }
        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
    }

    #[async_trait]
    impl LocalInferenceService for NoFim {
        async fn chat_completion(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<crate::openai_types::ChatCompletionResponse, crate::state::LocalInferenceError>
        {
            unimplemented!()
        }
        async fn chat_completion_stream(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = StreamFrame> + Send>>,
            crate::state::LocalInferenceError,
        > {
            unimplemented!()
        }
        fn provider_manifest(&self) -> Option<oicp_types::ProviderManifest> {
            None
        }
    }

    fn stub_frames() -> Vec<StreamFrame> {
        vec![
            StreamFrame::Token("x".into()),
            StreamFrame::Token(" + 1".into()),
            StreamFrame::Debug(serde_json::json!({"stop_rule": "stop_string"})),
            StreamFrame::Finish {
                reason: FinishReason::Stop,
                usage: Some(StreamUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                    total_tokens: 12,
                }),
            },
        ]
    }

    fn router_with(service: Arc<dyn LocalInferenceService>) -> axum::Router {
        let state = test_app_state_with_inference(service);
        crate::server::mock_router(state)
    }

    #[tokio::test]
    async fn rich_shape_non_stream_aggregates_and_carries_debug() {
        let seen = Arc::new(Mutex::new(None));
        let svc = Arc::new(StubFim {
            frames: stub_frames(),
            seen: seen.clone(),
        });
        let app = router_with(svc);
        let resp = app
            .oneshot(
                Request::post("/v1/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "prefix": "def add(a, b):\n    return a ",
                            "suffix": "\n",
                            "path": "math.py",
                            "debug": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["object"], "text_completion");
        assert_eq!(body["choices"][0]["text"], "x + 1");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["usage"]["total_tokens"], 12);
        assert_eq!(body["sovereign_debug"]["stop_rule"], "stop_string");
        // The seam saw the unified request.
        let got = seen.lock().unwrap().clone().expect("request recorded");
        assert!(got.prefix.starts_with("def add"));
        assert_eq!(got.suffix, "\n");
        assert_eq!(got.path.as_deref(), Some("math.py"));
    }

    #[tokio::test]
    async fn legacy_shape_maps_prompt_to_prefix() {
        let seen = Arc::new(Mutex::new(None));
        let svc = Arc::new(StubFim {
            frames: stub_frames(),
            seen: seen.clone(),
        });
        let app = router_with(svc);
        let resp = app
            .oneshot(
                Request::post("/v1/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "model": "qwen-coder-1.5b",
                            "prompt": "let x = ",
                            "suffix": ";",
                            "max_tokens": 16,
                            "stop": "\n\n"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["model"], "qwen-coder-1.5b");
        // No debug opt-in → no sovereign_debug key.
        assert!(body.get("sovereign_debug").is_none());
        let got = seen.lock().unwrap().clone().expect("request recorded");
        assert_eq!(got.prefix, "let x = ");
        assert_eq!(got.max_tokens, Some(16));
        assert_eq!(got.stop, vec!["\n\n".to_string()]);
    }

    #[tokio::test]
    async fn missing_prefix_is_400() {
        let app = router_with(Arc::new(NoFim));
        let resp = app
            .oneshot(
                Request::post("/v1/completions")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn default_impl_error_maps_to_503() {
        // NoFim uses the defaulted trait method, which errors.
        let app = router_with(Arc::new(NoFim));
        let resp = app
            .oneshot(
                Request::post("/v1/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prefix":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(resp.into_body()).await;
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not serve FIM"));
    }

    #[tokio::test]
    async fn streaming_emits_sse_chunks_terminal_reason_debug_and_done() {
        let svc = Arc::new(StubFim {
            frames: stub_frames(),
            seen: Arc::new(Mutex::new(None)),
        });
        let app = router_with(svc);
        let resp = app
            .oneshot(
                Request::post("/v1/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "prefix": "x = ",
                            "stream": true,
                            "debug": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        // Token chunks, terminal finish_reason, debug chunk, [DONE].
        assert!(
            text.contains("\"text\":\"x\""),
            "missing token chunk: {text}"
        );
        assert!(
            text.contains("\"text\":\" + 1\""),
            "missing 2nd chunk: {text}"
        );
        assert!(
            text.contains("\"finish_reason\":\"stop\""),
            "missing terminal: {text}"
        );
        assert!(
            text.contains("\"sovereign_debug\""),
            "missing debug chunk: {text}"
        );
        assert!(text.contains("[DONE]"), "missing [DONE]: {text}");
    }
}
