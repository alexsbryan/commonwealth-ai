use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use tracing::{debug, info, warn};

use commonwealth_core::ids::ModelId;
use commonwealth_inference::oicp::{self, CapabilityRequirements, ShardingPrivacy};

use crate::openai_types::*;
use crate::state::AppState;

/// POST /v1/chat/completions — OpenAI-compatible chat completions.
pub async fn chat_completions(
    State(state): State<AppState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    // Privacy enforcement: reject local_only requests.
    if let Some(ref oicp_req) = request.oicp {
        if oicp_req.sharding() == ShardingPrivacy::LocalOnly {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        "Requests with privacy 'local_only' must be handled by the client's \
                         local inference engine, not sent to Commonwealth. This is likely a \
                         client misconfiguration.",
                        "invalid_request_error",
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    }

    // --- Priority 0: in-process local inference ---
    //
    // When the daemon is embedded in Sovereign (sovereign-mesh),
    // `local_inference` wraps the same `EmbeddedLlamaCpp` the user
    // would use for a direct chat. Serve peer requests from it
    // first — cuts out the orchestrator path entirely and skips
    // the need for spawned llama-server processes.
    if let Some(service) = state.inner.local_inference.as_ref() {
        let want_stream = request.stream.unwrap_or(false);
        info!(
            want_stream,
            has_oicp = request.oicp.is_some(),
            "chat_completions: serving via local_inference"
        );
        if want_stream {
            return serve_local_stream(service.clone(), request).await;
        } else {
            return serve_local_non_stream(service.clone(), request).await;
        }
    }

    // --- Priority 1: Explicit OICP capability requirements ---
    if let Some(ref oicp_req) = request.oicp {
        if let Some(ref caps) = oicp_req.capabilities {
            let model_id = match route_with_oicp(&state, caps) {
                Some(id) => id,
                None => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(
                            serde_json::to_value(ErrorResponse::new(
                                "No loaded model satisfies the OICP requirements",
                                "model_not_available",
                            ))
                            .unwrap(),
                        ),
                    )
                        .into_response();
                }
            };
            return forward_to_model(&state, model_id, &request).await;
        }
    }

    // --- Priority 2: Model name matches a loaded model by name ---
    if let Some(ref requested_model) = request.model {
        if let Some(model_id) = find_model_by_name(&state, requested_model) {
            debug!(
                model_name = requested_model,
                "routing to model by exact name match"
            );
            return forward_to_model(&state, model_id, &request).await;
        }

        // --- Priority 3: Model name matches an alias → synthesize OICP ---
        if let Some(resolution) = state.inner.model_aliases.resolve(requested_model) {
            debug!(
                model_name = requested_model,
                "model name matched alias, synthesizing OICP requirements"
            );
            if let Some(model_id) = route_with_oicp(&state, &resolution.requirements) {
                return forward_to_model(&state, model_id, &request).await;
            }
        }
    }

    // --- Priority 4: Default model ---
    match state.default_model_id() {
        Some(model_id) => forward_to_model(&state, model_id, &request).await,
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ErrorResponse::new(
                    "No models are currently loaded on the mesh",
                    "model_not_available",
                ))
                .unwrap(),
            ),
        )
            .into_response(),
    }
}

fn route_with_oicp(state: &AppState, requirements: &CapabilityRequirements) -> Option<ModelId> {
    let models = state.inner.inference_store.list_models();
    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    let mut best_model = None;
    let mut best_score = -1.0f32;

    for shard_plan in &plan.model_plans {
        if let Some(model_info) = models.get(&shard_plan.model) {
            if oicp::satisfies_required(&model_info.oicp_capabilities, &requirements.required) {
                let score =
                    oicp::score_preferred(&model_info.oicp_capabilities, &requirements.preferred);
                if score > best_score {
                    best_score = score;
                    best_model = Some(shard_plan.model);
                }
            }
        }
    }

    best_model
}

fn find_model_by_name(state: &AppState, name: &str) -> Option<ModelId> {
    let models = state.inner.inference_store.list_models();
    let name_lower = name.to_lowercase();
    models
        .values()
        .find(|m| m.name.to_lowercase() == name_lower)
        .map(|m| m.id)
}

async fn forward_to_model(
    state: &AppState,
    model_id: ModelId,
    request: &ChatCompletionRequest,
) -> Response {
    let llama_addr = match state.get_llama_server_address(model_id) {
        Some(addr) => addr,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        "Model is scheduled but llama-server is not yet ready",
                        "model_not_ready",
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };

    let forward_body = serde_json::to_string(request).unwrap_or_default();
    match forward_to_llama_server(&llama_addr, &forward_body).await {
        Ok(response_body) => match serde_json::from_str::<serde_json::Value>(&response_body) {
            Ok(value) => (StatusCode::OK, Json(value)).into_response(),
            Err(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                [("retry-after", "10")],
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        "Invalid response from inference backend",
                        "backend_error",
                    ))
                    .unwrap(),
                ),
            )
                .into_response(),
        },
        Err(e) => {
            warn!(error = %e, "failed to forward to llama-server");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [("retry-after", "10")],
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        format!(
                            "Inference backend unavailable: {e}. \
                             The mesh is recovering — retry shortly."
                        ),
                        "backend_unavailable",
                    ))
                    .unwrap(),
                ),
            )
                .into_response()
        }
    }
}

async fn forward_to_llama_server(
    address: &str,
    body: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let stream = tokio::net::TcpStream::connect(address).await?;

    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: {address}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    stream.writable().await?;
    stream.try_write(request.as_bytes())?;

    let mut response = Vec::new();
    loop {
        stream.readable().await?;
        let mut buf = [0u8; 4096];
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }

    let response_str = String::from_utf8_lossy(&response);
    if let Some(body_start) = response_str.find("\r\n\r\n") {
        Ok(response_str[body_start + 4..].to_string())
    } else {
        Ok(response_str.to_string())
    }
}

/// GET /v1/models — list available models.
pub async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let models = state.inner.inference_store.list_models();
    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    let data: Vec<ModelObject> = models
        .values()
        .map(|model| {
            let shard_plan = plan.model_plans.iter().find(|p| p.model == model.id);
            let loaded = state
                .inner
                .inference_store
                .get_llama_address(model.id)
                .is_some();

            ModelObject {
                id: model.name.clone(),
                object: "model".into(),
                created: 0,
                owned_by: "mesh".into(),
                capabilities: Some(serde_json::to_value(&model.oicp_capabilities).unwrap()),
                performance: shard_plan.map(|p| ModelPerformance {
                    estimated_tokens_per_sec: p.estimated_tokens_per_sec,
                    estimated_ttft_ms: p.estimated_ttft_ms,
                    loaded,
                }),
            }
        })
        .collect();

    Json(ModelListResponse {
        object: "list".into(),
        data,
    })
}

// ── Local-inference serving helpers ────────────────────────────
//
// Delegate chat-completions to the AppState's `local_inference`
// hook (when present). Two shapes:
//
//   • non-streaming: hook returns `ChatCompletionResponse`, we emit
//     it verbatim as JSON. Matches the OpenAI baseline.
//   • streaming: hook returns `Stream<Item = Result<String>>` of
//     partial token chunks; we wrap each chunk in an OpenAI-format
//     SSE `data:` event with a `delta.content` payload. A final
//     `[DONE]` sentinel closes the stream, per the OpenAI spec.
//
// Streaming is mandatory because Sovereign's runtime uses
// `complete_stream` on the wire — without SSE here, the Joiner's
// `RemoteApiProvider::complete_stream` would error on parse.

async fn serve_local_non_stream(
    service: std::sync::Arc<dyn crate::state::LocalInferenceService>,
    request: ChatCompletionRequest,
) -> Response {
    match service.chat_completion(request).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => {
            warn!(error = %e, "chat_completions: local inference failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        format!("local inference failed: {e}"),
                        "backend_error",
                    ))
                    .unwrap(),
                ),
            )
                .into_response()
        }
    }
}

async fn serve_local_stream(
    service: std::sync::Arc<dyn crate::state::LocalInferenceService>,
    request: ChatCompletionRequest,
) -> Response {
    // `id` / `created` are placeholders that would match the
    // non-streaming response — clients that care about stable ids
    // can set them on their side; we follow the OpenAI convention
    // of `chatcmpl-*` + unix timestamp.
    let id = format!(
        "chatcmpl-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let model = request.model.clone().unwrap_or_else(|| "local".into());

    let token_stream = match service.chat_completion_stream(request).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "chat_completions: local stream failed to start");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        format!("local stream failed: {e}"),
                        "backend_error",
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
        }
    };

    // Translate token chunks → SSE events in the OpenAI
    // `chat.completion.chunk` shape. Per-frame allocation is fine:
    // at a few kilobytes per chunk for a handful of chunks per
    // second this is well inside what reqwest + axum handle.
    let id_for_stream = id.clone();
    let model_for_stream = model.clone();
    let sse_events = token_stream.map(move |item| match item {
        Ok(delta) => {
            let chunk = serde_json::json!({
                "id": id_for_stream,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_for_stream,
                "choices": [{
                    "index": 0,
                    "delta": { "content": delta },
                    "finish_reason": null
                }]
            });
            Ok::<_, std::convert::Infallible>(
                Event::default().data(chunk.to_string()),
            )
        }
        Err(e) => {
            // Surface the error as a final event then let the
            // stream close — clients handle the abrupt end.
            warn!(error = %e, "chat_completions: local stream chunk error");
            Ok(Event::default().data(format!(
                "{{\"error\":{{\"message\":\"{}\"}}}}",
                e.replace('"', "\\\"")
            )))
        }
    });

    // Append the OpenAI `[DONE]` sentinel so the consumer knows
    // the stream ended cleanly. `RemoteApiProvider::complete_stream`
    // explicitly breaks its loop on this marker.
    let done = futures::stream::once(async move {
        Ok::<_, std::convert::Infallible>(Event::default().data("[DONE]"))
    });
    let combined = sse_events.chain(done);

    Sse::new(combined)
        .keep_alive(KeepAlive::default())
        .into_response()
}
