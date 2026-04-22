use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use tracing::{debug, info, warn};

use commonwealth_core::ids::ModelId;
use commonwealth_inference::oicp::{
    self, CapabilityClaim, CapabilityHint, InferenceRequirements,
    LatencyClass, ShardingPrivacy,
};

use crate::middleware::{
    MiddlewareError, MiddlewareSession, Pipeline, PipelineContext, ResponseView,
};
use crate::openai_types::*;
use crate::state::AppState;

/// POST /v1/chat/completions — OpenAI-compatible chat completions.
pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Response {
    // ── ATOS pipeline resolution ──────────────────────────────────────
    //
    // If the model name matches a pipeline alias (e.g.,
    // `commonwealth/sovereign-coder`), run the ATOS middleware chain
    // before any legacy routing. On success the pipeline rewrites
    // `request.model` to the concrete model id the middleware config
    // says to use, and we drop into priority-0 routing exactly as if
    // the client had sent the concrete name directly.
    //
    // The pipeline also arms a `PostPathGuard` that spawns
    // `run_post` as a detached task when this handler returns — any
    // exit path triggers it via `Drop`, so the post-path fires
    // whether we serve from local_inference, fall through to OICP
    // routing, or return an error.
    //
    // On failure — the usual cause is ApprovalRequired when a
    // feature hasn't been approved and the request tries to call a
    // write-intent tool — we short-circuit with a structured
    // OpenAI-compatible error so opencode surfaces it as a model
    // error rather than a transport failure.
    let requested_model = request.model.clone().unwrap_or_default();
    let _post_guard: Option<PostPathGuard>;
    if let Some(pipeline_res) = state
        .inner
        .pipeline_aliases
        .resolve(&requested_model)
        .cloned()
    {
        match run_atos_pipeline(&state, &headers, &mut request, &pipeline_res).await {
            Ok(guard) => _post_guard = guard,
            Err(resp) => return resp,
        }
        // Rewrite the model field so downstream routing finds the
        // concrete model. Preserve the original only in a debug log.
        debug!(
            pipeline = %pipeline_res.name,
            target_model = %pipeline_res.model_id,
            "atos pipeline resolved; rewriting request.model"
        );
        request.model = Some(pipeline_res.model_id.clone());
    } else {
        _post_guard = None;
    }

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
    //
    // The client has opinions about capability hint, latency class,
    // or sizing. Rank every loaded model's synthesized claim against
    // the request and serve the best.
    if let Some(ref oicp_req) = request.oicp {
        let has_v03_routing = oicp_req.capability_hint.is_some()
            || oicp_req.latency_class.is_some()
            || oicp_req.context_tokens.is_some()
            || oicp_req.max_output_tokens.is_some();
        if has_v03_routing {
            let model_id = match route_with_oicp(&state, oicp_req) {
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
                alias_hint = %resolution.hint,
                alias_latency = ?resolution.latency_class,
                "model name matched alias, synthesizing OICP requirements"
            );
            let synthesized = InferenceRequirements::new()
                .with_hint(resolution.hint.clone())
                .with_latency_class(resolution.latency_class);
            if let Some(model_id) = route_with_oicp(&state, &synthesized) {
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

/// Rank every loaded model's synthesized v0.3 claim against the
/// request and return the `ModelId` with the highest score. Returns
/// `None` when no claim passes the hard gate.
fn route_with_oicp(
    state: &AppState,
    req: &InferenceRequirements,
) -> Option<ModelId> {
    let models = state.inner.inference_store.list_models();
    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    let mut best_model = None;
    let mut best_score = f32::NEG_INFINITY;

    for shard_plan in &plan.model_plans {
        let Some(model_info) = models.get(&shard_plan.model) else {
            continue;
        };
        let claim = synthesize_claim_for_model_info(model_info);
        if let Some(score) = oicp::score_claim_for_request(&claim, req) {
            if score > best_score {
                best_score = score;
                best_model = Some(shard_plan.model);
            }
        }
    }

    best_model
}

/// Synthesize a v0.3 [`CapabilityClaim`] for a loaded `ModelInfo`.
/// Mirrors the synthesis in `routes_oicp::synthesize_default_claim`
/// — name heuristic for code specialists + profile-derived affinity
/// — so the scheduler and advertiser agree on each model's claim
/// shape.
fn synthesize_claim_for_model_info(
    model_info: &commonwealth_inference::ModelInfo,
) -> CapabilityClaim {
    let name_lower = model_info.name.to_lowercase();
    let is_code_specialist = name_lower.contains("coder")
        || name_lower.contains("code-llama")
        || name_lower.contains("codellama")
        || name_lower.contains("deepseek-coder");

    let (hint, affinity) = if is_code_specialist {
        let code = oicp::proficiency(&model_info.oicp_capabilities, oicp::Capability::Code);
        (
            CapabilityHint::code(),
            (code as f32 / 4.0).clamp(0.0, 1.0),
        )
    } else {
        let best = [
            oicp::Capability::General,
            oicp::Capability::Analysis,
            oicp::Capability::Instruction,
        ]
        .into_iter()
        .map(|c| oicp::proficiency(&model_info.oicp_capabilities, c))
        .max()
        .unwrap_or(0);
        (
            CapabilityHint::general(),
            (best as f32 / 4.0).clamp(0.0, 1.0),
        )
    };

    CapabilityClaim::new(hint, LatencyClass::Normal, 32_768, 2_048, affinity)
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

// ─── ATOS pipeline helpers ───────────────────────────────────────────────────

/// Run the ATOS middleware chain against a request. Returns `Ok(())`
/// when the chain completes successfully; returns an
/// already-constructed HTTP `Response` when a middleware wants to
/// short-circuit (e.g., ApprovalRequired).
///
/// Persists session state back to MeshStore on exit so subsequent
/// requests on the same `X-Session-Id` see the mutations. Missing
/// prerequisites (no session_store, no X-Feature-Id) degrade the
/// call to a no-op — the handler then proceeds to legacy routing
/// with the unmodified request.
async fn run_atos_pipeline(
    state: &AppState,
    headers: &HeaderMap,
    request: &mut ChatCompletionRequest,
    pipeline: &commonwealth_core::pipeline_aliases::PipelineResolution,
) -> Result<Option<PostPathGuard>, Response> {
    let Some(session_store) = state.inner.session_store.clone() else {
        debug!("atos pipeline resolved but no session store configured; skipping middleware");
        return Ok(None);
    };

    // Extract headers. Strings are ASCII; non-ASCII values are
    // treated as absent rather than 400ing — a stray UTF-8 in the
    // header is likely a client bug and we'd rather degrade.
    let feature_id = headers
        .get("x-feature-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let session_id = headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uuid_like_for_sessionless());

    // Build the pipeline from the alias config + middleware
    // registry. Unknown middleware ids become 500s so a typo
    // doesn't silently skip an important step.
    let pipeline_exec = match state
        .inner
        .middleware_registry
        .build_pipeline(&pipeline.middleware)
    {
        Ok(p) => p,
        Err(e) => {
            return Err(atos_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "atos_pipeline_misconfigured",
                &e.to_string(),
            ));
        }
    };

    let ctx = PipelineContext {
        pipeline_name: pipeline.name.clone(),
        model_id: pipeline.model_id.clone(),
        context_config: pipeline.context.clone(),
        feature_id: feature_id.clone(),
        session_id: Some(session_id.clone()),
        repo_root: state
            .inner
            .repo_root
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from(".")),
    };

    let mut handle = session_store.load_and_lock(&session_id).await;
    // Seed the feature_id from the header so middleware see it
    // immediately on first contact. ApprovalGate will overwrite
    // `approval_validated` / `spec_content_hash` as it runs.
    if handle.state.feature_id.is_none() {
        handle.state.feature_id = feature_id.clone();
    }

    // Mirror session state into the middleware-visible struct so
    // middleware don't depend on the full AtosSessionState type.
    let mut mw_session = MiddlewareSession {
        feature_id: handle.state.feature_id.clone(),
        approval_validated: handle.state.approval_validated,
        spec_content_hash: handle.state.spec_content_hash.clone(),
        pending_deviation_ack: handle.state.pending_deviation_ack,
        deviation_note_id: handle.state.deviation_note_id.clone(),
        pending_artifact_delta: handle.state.pending_artifact_delta.clone(),
        last_seen_at: handle.state.last_seen_at,
    };

    let outcome = pipeline_exec.run(request, &mut mw_session, &ctx).await;

    // Copy mw_session mutations back into the persistent state
    // before saving.
    handle.state.feature_id = mw_session.feature_id;
    handle.state.approval_validated = mw_session.approval_validated;
    handle.state.spec_content_hash = mw_session.spec_content_hash;
    handle.state.pending_deviation_ack = mw_session.pending_deviation_ack;
    handle.state.deviation_note_id = mw_session.deviation_note_id;
    handle.state.pending_artifact_delta = mw_session.pending_artifact_delta;
    handle.save().await;

    match outcome {
        Ok(()) => Ok(Some(PostPathGuard {
            pipeline: std::sync::Arc::new(pipeline_exec),
            ctx: std::sync::Arc::new(ctx),
            session_store,
            session_id,
        })),
        Err(err) => Err(middleware_error_to_response(err)),
    }
}

/// RAII guard that spawns the post-path middleware chain as a
/// detached `tokio::spawn` when dropped.
///
/// Why a guard rather than an explicit call at every handler exit:
/// `chat_completions` has many return paths (privacy reject, OICP
/// unavailable, forward_to_model return, local_inference return,
/// streaming SSE). A drop guard catches all of them with one
/// insertion point. The spawn runs AFTER the response is on the
/// wire — the client never waits on post-path telemetry.
///
/// Concurrency: the per-session mutex lives in `SessionStore`, so
/// if the next request's pre-path arrives before post-path
/// completes, the pre-path waits. That's the desired ordering —
/// post-path mutations are visible to the next turn.
pub(crate) struct PostPathGuard {
    pipeline: std::sync::Arc<Pipeline>,
    ctx: std::sync::Arc<PipelineContext>,
    session_store: sovereign_atos::session::SessionStore,
    session_id: String,
}

impl Drop for PostPathGuard {
    fn drop(&mut self) {
        let pipeline = self.pipeline.clone();
        let ctx = self.ctx.clone();
        let store = self.session_store.clone();
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            // Re-load session state so post-path sees whatever MCP
            // tool calls wrote during the turn. Pre-path's save
            // already committed its mutations; we pick up from
            // there.
            let mut handle = store.load_and_lock(&session_id).await;
            let mut mw_session = MiddlewareSession {
                feature_id: handle.state.feature_id.clone(),
                approval_validated: handle.state.approval_validated,
                spec_content_hash: handle.state.spec_content_hash.clone(),
                pending_deviation_ack: handle.state.pending_deviation_ack,
                deviation_note_id: handle.state.deviation_note_id.clone(),
                pending_artifact_delta: handle.state.pending_artifact_delta.clone(),
                last_seen_at: handle.state.last_seen_at,
            };
            // For M5.1 we pass a synthetic empty response view.
            // M5.2's ArtifactSurface reads from the DB, not from
            // `content`, so this is sufficient. A future
            // enhancement would capture the real response bytes.
            let view = ResponseView {
                content: "",
                finish_reason: Some("stop"),
                tool_calls_emitted: 0,
            };
            pipeline.run_post(&view, &mut mw_session, &ctx).await;
            // Copy back + persist.
            handle.state.feature_id = mw_session.feature_id;
            handle.state.approval_validated = mw_session.approval_validated;
            handle.state.spec_content_hash = mw_session.spec_content_hash;
            handle.state.pending_deviation_ack = mw_session.pending_deviation_ack;
            handle.state.deviation_note_id = mw_session.deviation_note_id;
            handle.state.pending_artifact_delta = mw_session.pending_artifact_delta;
            handle.save().await;
        });
    }
}

/// Generate a per-request pseudo-session id when the client didn't
/// send an `X-Session-Id` header. Ephemeral; not persisted beyond
/// the in-memory mutex lifetime because the next request without a
/// header makes a new one.
fn uuid_like_for_sessionless() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sessionless-{nanos:x}")
}

fn middleware_error_to_response(err: MiddlewareError) -> Response {
    match err {
        MiddlewareError::ApprovalRequired { feature_id, hint } => atos_error_response(
            StatusCode::FORBIDDEN,
            "atos_approval_required",
            &format!("feature '{feature_id}' is not approved: {hint}"),
        ),
        MiddlewareError::PipelineRejected(msg) => atos_error_response(
            StatusCode::FORBIDDEN,
            "atos_pipeline_rejected",
            &msg,
        ),
        MiddlewareError::Infra(msg) => atos_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "atos_pipeline_infra_error",
            &msg,
        ),
    }
}

fn atos_error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let envelope = serde_json::json!({
        "error": {
            "message": message,
            "type": code,
            "code": code,
        }
    });
    (status, Json(envelope)).into_response()
}
