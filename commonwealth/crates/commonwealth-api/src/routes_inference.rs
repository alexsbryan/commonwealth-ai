use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use tracing::{debug, info, warn};

use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::mesh::NodeStatus;
use commonwealth_inference::oicp::{
    self, CapabilityClaim, CapabilityHint, InferenceRequirements,
    LatencyClass, ShardingPrivacy,
};
use std::collections::HashSet;
use std::time::Instant;

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
    // ── Foreground-yield bump ─────────────────────────────────────────
    //
    // Fire-and-forget atomic store of the current unix-ts. The
    // corpus-engine ingest pipeline polls this through a `YieldHook`
    // before each embed batch / enrichment phase and pauses while
    // the timestamp is recent. This is the *only* bump site for
    // foreground activity — every chat path (HTTP, Tauri proxy,
    // CLI `sovereign chat`, MCP `tools/call` that triggers chat)
    // converges through this handler before slot dispatch, so a
    // single store covers them all. The corresponding bump for
    // `/v1/embeddings` is intentionally absent: embed requests come
    // from ingest itself; bumping there would prevent self-yield.
    state.bump_foreground_active();

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

    // Slot-alias rewrite. `commonwealth/primary` / `primary` etc.
    // resolve to the GGUF stem bound to that slot in `SetupConfig`,
    // so opencode (and other OpenAI-shape clients) can name slots
    // instead of GGUF filenames. Pipeline aliases below still take
    // precedence — a client that explicitly names a pipeline alias
    // gets the full middleware stack rather than the bare slot.
    if let Some(slot_target) = state.resolve_slot_alias(&requested_model) {
        debug!(
            requested = %requested_model,
            target = %slot_target,
            "chat_completions: slot alias resolved"
        );
        request.model = Some(slot_target);
    }

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

    // --- Priority 0: in-process local inference ---
    //
    // When the daemon is embedded in Sovereign (sovereign-mesh),
    // `local_inference` wraps the same `EmbeddedLlamaCpp` the user
    // would use for a direct chat. Serve peer requests from it
    // first — cuts out the orchestrator path entirely and skips
    // the need for spawned llama-server processes.
    //
    // local_inference is local by definition, so it satisfies the
    // OICP `LocalOnly` privacy default (§3.1) without any further
    // gate. The privacy enforcement below intercepts only the
    // forward-to-mesh / forward-to-Commonwealth paths where a
    // request *would* leave this machine.
    if let Some(service) = state.inner.local_inference.as_ref() {
        let want_stream = request.stream.unwrap_or(false);
        info!(
            want_stream,
            has_oicp = request.oicp.is_some(),
            "chat_completions: serving via local_inference"
        );
        let requester = crate::headers::parse_x_node_id(&headers);
        let model_id = request.model.clone().unwrap_or_else(|| "local".into());
        if want_stream {
            return serve_local_stream(
                service.clone(),
                request,
                state.clone(),
                requester,
                model_id,
            )
            .await;
        } else {
            return serve_local_non_stream(
                service.clone(),
                request,
                state.clone(),
                requester,
                model_id,
            )
            .await;
        }
    }

    // Privacy enforcement at the forwarding boundary.
    //
    // We've already given local_inference (Priority 0) first refusal.
    // If we're still here, this request will leave the machine — the
    // OICP routing path can pick a peer model, and the legacy
    // forward_to_model fall-through targets a non-local backend. Per
    // OICP §3.1 the privacy default is `LocalOnly`, and the contract
    // is that LocalOnly requests must NOT cross the trust boundary.
    // Reject here so a misconfigured client can't accidentally leak
    // by sending LocalOnly past the local serving path.
    if let Some(ref oicp_req) = request.oicp {
        if oicp_req.sharding() == ShardingPrivacy::LocalOnly {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        "Requests with privacy 'local_only' cannot be forwarded — no \
                         local inference path is available to serve them. Either load a \
                         local model on this node or relax the privacy requirement.",
                        "invalid_request_error",
                    ))
                    .unwrap(),
                ),
            )
                .into_response();
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

/// POST /v1/embeddings — OpenAI-compatible embeddings endpoint.
///
/// Delegates to `local_inference.embed(input)`. The request's `model`
/// field is accepted and echoed back in the response envelope but
/// does not drive routing today — the embedding slot wired into the
/// local-inference service is whatever model the daemon has loaded
/// for embeddings. When a future release supports multiple embed
/// models, this handler will grow model-name dispatch the way
/// chat_completions does.
pub async fn embeddings(
    State(state): State<AppState>,
    Json(request): Json<EmbeddingRequest>,
) -> Response {
    let Some(service) = state.inner.local_inference.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ErrorResponse::new(
                    "this daemon has no local embedding backend — start it with \
                     sovereign-mesh so an EmbedSlot is attached, or route to a peer \
                     that advertises embeddings",
                    "no_local_embedding_backend",
                ))
                .unwrap(),
            ),
        )
            .into_response();
    };

    // Fan out over single-or-batch input. Each call is independent;
    // a future pass can add `embed_batch` for backends that batch
    // more efficiently than one-at-a-time.
    let inputs: Vec<String> = match request.input {
        EmbeddingInput::Single(s) => vec![s],
        EmbeddingInput::Batch(v) => v,
    };
    if inputs.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::to_value(ErrorResponse::new(
                    "embeddings request: `input` must be a non-empty string or array",
                    "invalid_request_error",
                ))
                .unwrap(),
            ),
        )
            .into_response();
    }

    let mut data: Vec<EmbeddingData> = Vec::with_capacity(inputs.len());
    let mut total_chars: usize = 0;
    for (i, text) in inputs.into_iter().enumerate() {
        total_chars += text.len();
        match service.embed(&text).await {
            Ok(vec) => data.push(EmbeddingData {
                object: "embedding".into(),
                embedding: vec,
                index: i,
            }),
            Err(e) => {
                warn!(error = %e, index = i, "embeddings: local embed failed");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        serde_json::to_value(ErrorResponse::new(
                            format!("embedding[{i}] failed: {e}"),
                            "backend_error",
                        ))
                        .unwrap(),
                    ),
                )
                    .into_response();
            }
        }
    }

    // The OpenAI spec counts token usage; we only have char count, so
    // we produce a conservative ~4 chars/token estimate rather than
    // leaving the field out (some clients require it to be present).
    let approx_tokens = ((total_chars + 3) / 4) as u32;
    let resp = EmbeddingResponse {
        object: "list".into(),
        data,
        model: request.model,
        usage: Usage {
            prompt_tokens: approx_tokens,
            completion_tokens: 0,
            total_tokens: approx_tokens,
        },
    };
    (StatusCode::OK, Json(resp)).into_response()
}

/// GET /v1/models — list models the daemon can actually serve right now.
///
/// Filters out entries whose owning peer is currently unreachable in the
/// mesh: the `inference_store` accumulates every model any peer has
/// gossiped, but if the advertising peer is offline a chat-completions
/// request targeting that model would 503 with no fallback. The contract
/// for `/v1/models` is "what the daemon can route this instant," so we
/// drop the unreachable ones rather than make callers do liveness
/// guessing themselves.
///
/// A model is kept when either:
///   - its store entry was last written by an online peer (or by us), or
///   - the local daemon currently has the model loaded
///     (covers the case where the latest gossip overwrote our entry's
///     origin with a now-offline peer's NodeId, even though we still
///     hold the weights).
pub async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let local_id = state.self_node_id();
    let live_nodes: HashSet<NodeId> = {
        let mesh = state.inner.mesh.read().await;
        std::iter::once(local_id)
            .chain(
                mesh.members
                    .values()
                    .filter(|m| {
                        matches!(m.status, NodeStatus::Online | NodeStatus::Busy)
                    })
                    .map(|m| m.node_id),
            )
            .collect()
    };

    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    let mut data: Vec<ModelObject> = state
        .inner
        .inference_store
        .list_models_with_origins()
        .into_iter()
        .filter(|(origin, model)| {
            live_nodes.contains(origin)
                || state
                    .inner
                    .inference_store
                    .get_llama_address(model.id)
                    .is_some()
        })
        .map(|(_, model)| {
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

    // Append synthetic entries for each registered slot alias so
    // discovery surfaces (opencode's model picker, /v1/models curl)
    // see the slot names alongside the GGUF stems. These are
    // dereferenced server-side at request time — no client config
    // churn when an operator swaps GGUFs in `[models]`.
    let slot_aliases = state.inner.slot_aliases.load();
    let mut alias_entries: Vec<(String, String)> = slot_aliases
        .iter()
        .map(|(alias, target)| (alias.clone(), target.clone()))
        .collect();
    alias_entries.sort();
    for (alias, target) in alias_entries {
        // Skip namespaced duplicates when the bare form already
        // appears — opencode treats them as separate ids and we want
        // both visible (operator typing `commonwealth/primary` finds
        // the namespaced entry; bare CLI users find `primary`).
        let owned_by = format!("alias→{target}");
        data.push(ModelObject {
            id: alias,
            object: "model".into(),
            created: 0,
            owned_by,
            capabilities: None,
            performance: None,
        });
    }

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
    state: AppState,
    requester: Option<NodeId>,
    model_id: String,
) -> Response {
    let started = Instant::now();
    match service.chat_completion(request).await {
        Ok(resp) => {
            // Emit `InferenceServed` only when the request was
            // attributed to a remote mesh peer via `X-Node-Id`. The
            // ledger explicitly tracks intra-mesh activity (spec §10
            // scope exclusion: cross-mesh / desktop-local requests
            // do not accumulate dimensional contribution data).
            if let Some(for_node) = requester {
                let tokens =
                    resp.usage.as_ref().map(|u| u.completion_tokens as u64).unwrap_or(0);
                let wall_seconds = started.elapsed().as_secs_f64();
                state.inner.contribution_emitter.record(
                    LedgerEventKind::InferenceServed {
                        for_node,
                        model_id,
                        tokens_generated: tokens,
                        wall_seconds,
                    },
                );
            }
            (StatusCode::OK, Json(resp)).into_response()
        }
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
    state: AppState,
    requester: Option<NodeId>,
    model_id_for_ledger: String,
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

    // Per-stream counters for the InferenceServed ledger emission
    // when the stream completes. Wrapping the underlying stream in
    // a `scan` adapter keeps the count + start-time in scope until
    // the stream closes, at which point the `done` future emits the
    // ledger event in a tokio task.
    let chunks_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let started = Instant::now();

    let id_for_stream = id.clone();
    let model_for_stream = model.clone();
    let chunks_count_for_stream = chunks_count.clone();
    let sse_events = token_stream.map(move |frame| {
        use crate::openai_types::StreamFrame;
        match frame {
            StreamFrame::Token(delta) => {
                chunks_count_for_stream
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            StreamFrame::ToolCalls(calls) => {
                // Synthetic tools-streaming chunk. Local backends
                // parse `<tool_call>` markup post-generation, so we
                // emit one chunk carrying every parsed call rather
                // than the per-fragment `arguments` deltas the
                // OpenAI spec also permits. Both shapes are
                // wire-legal — clients accumulate by `tool_calls[i].
                // index` regardless of chunk count.
                chunks_count_for_stream
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let tool_calls_json: Vec<serde_json::Value> = calls
                    .iter()
                    .enumerate()
                    .map(|(i, c)| serde_json::json!({
                        "index": i,
                        "id": c.id,
                        "type": c.kind,
                        "function": {
                            "name": c.function.name,
                            "arguments": c.function.arguments,
                        }
                    }))
                    .collect();
                let chunk = serde_json::json!({
                    "id": id_for_stream,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model_for_stream,
                    "choices": [{
                        "index": 0,
                        "delta": {
                            "role": "assistant",
                            "tool_calls": tool_calls_json,
                        },
                        "finish_reason": null
                    }]
                });
                Ok::<_, std::convert::Infallible>(
                    Event::default().data(chunk.to_string()),
                )
            }
            StreamFrame::Finish { reason, usage } => {
                // Terminal frame: emit an OpenAI-shaped chunk with
                // an empty delta and the real `finish_reason`. This
                // is the bug fix that motivated the typed surface —
                // the legacy `Result<String>` couldn't carry the
                // signal so every truncation looked like a clean
                // stop on the wire.
                let mut payload = serde_json::json!({
                    "id": id_for_stream,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model_for_stream,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": reason.as_openai_str()
                    }]
                });
                if let Some(u) = usage {
                    payload["usage"] = serde_json::json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens,
                    });
                }
                Ok(Event::default().data(payload.to_string()))
            }
            StreamFrame::Error(e) => {
                // Surface the error as a final event then let the
                // stream close — clients handle the abrupt end.
                warn!(error = %e, "chat_completions: local stream error frame");
                Ok(Event::default().data(format!(
                    "{{\"error\":{{\"message\":\"{}\"}}}}",
                    e.replace('"', "\\\"")
                )))
            }
        }
    });

    // Append the OpenAI `[DONE]` sentinel so the consumer knows
    // the stream ended cleanly. `RemoteApiProvider::complete_stream`
    // explicitly breaks its loop on this marker.
    //
    // We piggy-back the `done` future to emit the `InferenceServed`
    // ledger event for cross-mesh requests: token count is the
    // chunk counter (each SSE frame ≈ one model token in the
    // llama.cpp stream), wall_seconds is real elapsed since
    // dispatch. Local-origin streams (no `X-Node-Id`) skip the
    // emission, matching the non-streaming policy.
    let chunks_for_done = chunks_count;
    let state_for_done = state.inner.contribution_emitter.clone();
    let requester_for_done = requester;
    let model_for_done = model_id_for_ledger;
    let done = futures::stream::once(async move {
        if let Some(for_node) = requester_for_done {
            let tokens =
                chunks_for_done.load(std::sync::atomic::Ordering::Relaxed);
            let wall_seconds = started.elapsed().as_secs_f64();
            state_for_done.record(LedgerEventKind::InferenceServed {
                for_node,
                model_id: model_for_done,
                tokens_generated: tokens,
                wall_seconds,
            });
        }
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
        pending_decision: handle.state.pending_decision.clone(),
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
    handle.state.pending_decision = mw_session.pending_decision;
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
                pending_decision: handle.state.pending_decision.clone(),
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
            handle.state.pending_decision = mw_session.pending_decision;
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
