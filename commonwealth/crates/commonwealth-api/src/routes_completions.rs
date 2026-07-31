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
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;

use crate::openai_types::{CompletionsRequestWire, ErrorResponse, StopParam, StreamFrame};
use crate::state::{AppState, FimCompletionRequest, FimStreamStart};

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

    let Some(service) = state.inner.local_inference.as_ref() else {
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

/// Envelope ids follow the OpenAI convention (`cmpl-*` + ms epoch).
fn completion_id() -> String {
    format!(
        "cmpl-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Non-streaming: consume the whole stream, aggregate the text, and
/// return one OpenAI `text_completion` object. `sovereign_debug` is
/// attached when (and only when) the request opted in.
async fn serve_fim_aggregated(
    start: FimStreamStart,
    debug_wanted: bool,
    model_echo: Option<String>,
) -> Response {
    let FimStreamStart {
        stream,
        model_id,
        slot: _,
        fim_style: _,
    } = start;
    let mut text = String::new();
    let mut finish_reason = "stop".to_string();
    let mut usage = None;
    let mut debug_payload = None;
    let mut stream = Box::pin(stream);
    while let Some(frame) = stream.next().await {
        match frame {
            StreamFrame::Token(t) => text.push_str(&t),
            StreamFrame::Finish { reason, usage: u } => {
                finish_reason = reason.as_openai_str().to_string();
                usage = u;
            }
            StreamFrame::Error(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::to_value(ErrorResponse::new(
                            format!("generation failed mid-stream: {e}"),
                            "backend_error",
                        ))
                        .unwrap_or_default(),
                    ),
                )
                    .into_response();
            }
            StreamFrame::Debug(v) => debug_payload = Some(v),
            StreamFrame::ToolCalls(_) => {
                // Never produced by the FIM adapter; ignore defensively.
            }
        }
    }
    let mut body = serde_json::json!({
        "id": completion_id(),
        "object": "text_completion",
        "created": unix_now(),
        "model": model_echo.unwrap_or(model_id),
        "choices": [{
            "text": text,
            "index": 0,
            "finish_reason": finish_reason,
        }],
    });
    if let Some(u) = usage {
        body["usage"] = serde_json::json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
        });
    }
    if debug_wanted {
        if let Some(d) = debug_payload {
            body["sovereign_debug"] = d;
        }
    }
    Json(body).into_response()
}

/// Streaming: bridge frames to SSE chunks (`text_completion` object
/// shape), a terminal chunk carrying the real `finish_reason` (+ the
/// opt-in `sovereign_debug`), then the `[DONE]` sentinel.
fn serve_fim_sse(
    start: FimStreamStart,
    debug_wanted: bool,
    model_echo: Option<String>,
) -> Response {
    let id = completion_id();
    let created = unix_now();
    let model = model_echo.unwrap_or_else(|| start.model_id.clone());

    let chunks = start.stream.map(move |frame| {
        let id = id.clone();
        let model = model.clone();
        match frame {
            StreamFrame::Token(delta) => {
                let chunk = serde_json::json!({
                    "id": id,
                    "object": "text_completion",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "text": delta,
                        "finish_reason": null
                    }]
                });
                Ok::<_, std::convert::Infallible>(Event::default().data(chunk.to_string()))
            }
            StreamFrame::Finish { reason, usage } => {
                let mut chunk = serde_json::json!({
                    "id": id,
                    "object": "text_completion",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "text": "",
                        "finish_reason": reason.as_openai_str()
                    }]
                });
                if let Some(u) = usage {
                    chunk["usage"] = serde_json::json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens,
                    });
                }
                Ok(Event::default().data(chunk.to_string()))
            }
            StreamFrame::Debug(v) => {
                if debug_wanted {
                    let chunk = serde_json::json!({
                        "id": id,
                        "object": "text_completion",
                        "created": created,
                        "model": model,
                        "choices": [],
                        "sovereign_debug": v,
                    });
                    Ok(Event::default().data(chunk.to_string()))
                } else {
                    // Opted-out debug frames vanish — the comment event
                    // keeps the stream well-formed without payload.
                    Ok(Event::default().comment("debug dropped"))
                }
            }
            StreamFrame::Error(e) => Ok(Event::default().data(format!(
                "{{\"error\":{{\"message\":\"{}\"}}}}",
                e.replace('"', "\\\"")
            ))),
            StreamFrame::ToolCalls(_) => {
                // Never produced by the FIM adapter.
                Ok(Event::default().comment("tool_calls dropped"))
            }
        }
    });
    let done = futures::stream::once(async {
        Ok::<_, std::convert::Infallible>(Event::default().data("[DONE]"))
    });
    Sse::new(chunks.chain(done))
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::{FinishReason, StreamUsage};
    use crate::state::{test_app_state, FimSlotStatus, FimStreamStart, LocalInferenceService};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use futures::Stream;
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
    impl LocalInferenceService for StubFim {
        async fn chat_completion(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<crate::openai_types::ChatCompletionResponse, String> {
            unimplemented!("chat not used in these tests")
        }
        async fn chat_completion_stream(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String> {
            unimplemented!("chat not used in these tests")
        }
        fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
            None
        }
        async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
            unimplemented!()
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
        fn fim_status(&self) -> Option<FimSlotStatus> {
            Some(FimSlotStatus {
                slot: "fim".into(),
                model_id: "qwen-coder-1.5b".into(),
                fim_style: "qwen_coder".into(),
                aliased_to_fast: false,
                next_edit_format: "region_instruct".into(),
            })
        }
    }

    struct NoFim;
    #[async_trait]
    impl LocalInferenceService for NoFim {
        async fn chat_completion(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<crate::openai_types::ChatCompletionResponse, String> {
            unimplemented!()
        }
        async fn chat_completion_stream(
            &self,
            _r: crate::openai_types::ChatCompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String> {
            unimplemented!()
        }
        fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
            None
        }
        async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
            unimplemented!()
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
        let state = test_app_state().with_local_inference(service);
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
