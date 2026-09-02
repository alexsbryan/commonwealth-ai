// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use tracing::{debug, info, warn};

use commonwealth_core::activity::{ActivityEventKind, ServedFor};
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::mesh::NodeStatus;
use commonwealth_inference::oicp::{self, CapabilityClaim, InferenceRequirements, ShardingPrivacy};
use std::collections::HashSet;
use std::time::Instant;

#[cfg(feature = "atos")]
use crate::middleware::{
    MiddlewareError, MiddlewareSession, Pipeline, PipelineContext, ResponseView,
};
use crate::openai_types::*;
use crate::state::AppState;

/// How this node names itself in `ModelObject::advertised_by`. A literal,
/// not the mesh member name: the caller is talking TO this daemon, so
/// "local" is the fact that distinguishes a slot here from a peer's, and it
/// stays true when the operator renames the node.
const LOCAL_HOLDER: &str = "local";

/// POST /v1/chat/completions — OpenAI-compatible chat completions.
pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    guest: Option<axum::Extension<crate::client_auth::Guest>>,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Response {
    // ── Guest scope refinement ────────────────────────────────────────
    //
    // `client_auth` already decided this caller may reach this ROUTE. What
    // it cannot decide is which MODEL, because that is in the body. So the
    // per-request half of `Scope::Models` lives here, next to the handler
    // that serves it — a future scope refines against its own handler, not
    // this one.
    //
    // This must REFUSE, never fall through. Below, an absent or unmatched
    // `model` walks down to Priority 4 and gets `default_model_id()`, which
    // for a guest would mean: asked for the model they were granted, got a
    // different one, HTTP 200, no way to tell. That is §18.3's `d45489a3`
    // verbatim — same model string, seconds apart, served by something else.
    if let Some(axum::Extension(crate::client_auth::Guest(grant))) = guest.as_ref() {
        let named = request.model.as_deref().map(str::trim).unwrap_or("");
        if !grant.allows_model(named) {
            let asked = if named.is_empty() {
                "no model named".to_string()
            } else {
                format!("model '{named}'")
            };
            warn!(
                asked = %named,
                grants = %grant.summary(),
                "chat_completions: refusing a guest request outside its grant"
            );
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": {
                        "message": format!(
                            "this guest link does not cover {asked} — it grants: {}. \
                             Name one of those in `model`.",
                            grant.summary()
                        ),
                        "type": "guest_scope",
                        "code": "model_not_granted",
                    }
                })),
            )
                .into_response();
        }
    }
    // ── Foreground-yield bump ─────────────────────────────────────────
    //
    // Fire-and-forget atomic store of the current unix-ts. The
    // corpus-engine ingest pipeline polls this through a `YieldHook`
    // before each embed batch / enrichment phase and pauses while
    // the timestamp is recent. This WAS claimed to be the only bump
    // site, on the belief that every chat path converged here; the
    // in-process paths (CLI `sovereign chat`, the server's turn
    // routes, MCP) never do, and measured 2026-09-02 they ran with the
    // timestamp still at 0. The structural site is now the turn
    // itself: every `Runtime` stream handle holds a `ForegroundLease`
    // on the corpus engine for the turn's whole life. This bump stays
    // for the external client whose request arrives here. The corresponding bump for
    // `/v1/embeddings` is intentionally absent: embed requests come
    // from ingest itself; bumping there would prevent self-yield.
    state.bump_foreground_active();

    // ── Tool-profile header → request field ──────────────────────────
    //
    // `X-Sovereign-Tool-Profile: <name>` lets per-request callers pick
    // a daemon-configured tool profile (defined in
    // `~/.svrnmesh/tool_profiles.toml`). The downstream inference
    // adapter consults `sovereign_mesh::tool_profile::global()` and
    // filters `request.tools[]` accordingly. We surface the value
    // here, not deeper, because route handlers own header access; the
    // service layer only sees the request body.
    //
    // Header values that aren't valid UTF-8 are dropped silently so a
    // misbehaving client can't poison the request with header bytes.
    if let Some(name) = headers
        .get("x-sovereign-tool-profile")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        request.tool_profile = Some(name.to_string());
    }

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
    //
    // EXCEPTION: the mesh-routable forms (`primary`, `commonwealth/primary`,
    // `fast`, `commonwealth/fast`) are passed through *unresolved* so
    // the mesh-inference layer's `locate_named_model` can see them as
    // routing targets — both this node and any peer that loaded a Slow
    // slot advertise the alias in their OICP manifest, so leaving the
    // alias in the request lets the load-balancer pick whichever node
    // is less busy. The alias is resolved to the local GGUF later,
    // inside `MeshInferenceProvider::complete`, but only on the branch
    // that actually serves locally. Resolving here (before routing)
    // pinned every call to whichever node had that specific GGUF id,
    // and the moment one peer swapped quants every cross-mesh request
    // failed with `Model not loaded`.
    let is_mesh_routable_alias = matches!(
        requested_model.as_str(),
        "primary" | "commonwealth/primary" | "fast" | "commonwealth/fast"
    );
    if !is_mesh_routable_alias {
        if let Some(slot_target) = state.resolve_slot_alias(&requested_model) {
            debug!(
                requested = %requested_model,
                target = %slot_target,
                "chat_completions: slot alias resolved"
            );
            request.model = Some(slot_target);
        }
    } else {
        debug!(
            requested = %requested_model,
            "chat_completions: mesh-routable alias — deferring resolution to mesh layer"
        );
    }

    // ATOS served-middleware pipeline (feature `atos`, off by default). Only
    // pipeline-alias requests (e.g. `commonwealth/sovereign-coder`) enter it;
    // plain `primary`/`fast`/concrete-model chat — the harness-protocol product
    // path — never resolves a pipeline and is unaffected when atos is off.
    #[cfg(feature = "atos")]
    let requested_model = request.model.clone().unwrap_or_default();
    #[cfg(feature = "atos")]
    let _post_guard: Option<PostPathGuard>;
    #[cfg(feature = "atos")]
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
        // Pre-generation chat-side reshape. Three surgical nudges,
        // each targeting a distinct failure mode observed in the
        // gym fixtures:
        //   1. Failure-recovery: tail is a tool result with non-zero
        //      exit → delete the failed call from history, inject
        //      banner + nudge with concrete alternatives. Targets
        //      verbatim retry attractor (gym 002).
        //   2. Anti-repetition: tail shows N+ identical exec_command
        //      emissions → nudge for strategy change. Targets the
        //      multi-turn loop attractor (gym 004).
        //   3. Read-attractor: ≥3 read-only commands in history and
        //      zero action commands → nudge naming apply_patch as
        //      the required next emission. Targets exploration-mode
        //      lock-in (gym 006).
        // All three share an idempotency gate so only one fires at a
        // time. Order matters: failure-recovery is most specific (a
        // single recent failure), anti-rep is intermediate (a pattern),
        // read-attractor is the most general (a mode).
        // Opt-OUT, default on: see `turn_fidelity::reshape_enabled`. All
        // three key on the Codex/opencode contract, so a client with a
        // different tool vocabulary never trips them — but an operator
        // running a shared anchor node can still say "serve it
        // unmodified" with SOVEREIGN_FRONTDOOR_RESHAPE=0.
        if crate::turn_fidelity::reshape_enabled() {
            crate::frontdoor::apply_failure_nudge_chat(&mut request);
            crate::frontdoor::apply_anti_repetition_chat(&mut request);
            crate::frontdoor::apply_read_attractor_nudge_chat(&mut request);
        }
        // Citation-allowlist accumulators — OPT-IN, default off. Both
        // turn things seen in `role: tool` messages into SAMPLER
        // CONSTRAINTS, which is right for a retrieval-synthesis turn
        // and wrong for a general OpenAI client. A caller-supplied
        // allowlist is untouched either way. Rationale, measurement and
        // flip condition: `turn_fidelity::auto_allowlist_enabled`.
        if crate::turn_fidelity::auto_allowlist_enabled() {
            crate::frontdoor::apply_url_allowlist_from_tool_results(&mut request);
            crate::frontdoor::apply_evidence_id_allowlist_from_tool_results(&mut request);
            debug!(
                urls = request.url_allowlist.as_ref().map(|u| u.len()).unwrap_or(0),
                evidence_ids = request
                    .evidence_id_allowlist
                    .as_ref()
                    .map(|e| e.len())
                    .unwrap_or(0),
                "chat_completions: auto-allowlist synthesis is ON"
            );
        }
        let want_stream = request.stream.unwrap_or(false);
        // debug!, not info!: per-request entry breadcrumb. The served
        // request is still summarised once at INFO by `inference.complete:
        // done` (sovereign-inference engine.rs) — keeping both at INFO put
        // 8 lines per request in the log, and a ~37s synthetic keepalive
        // probe turned that into thousands of lines/day (2026-07-18).
        debug!(
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
                    .unwrap_or_default(),
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
                            .unwrap_or_default(),
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
                .unwrap_or_default(),
            ),
        )
            .into_response(),
    }
}

/// Rank every loaded model's synthesized v0.3 claim against the
/// request and return the `ModelId` with the highest score. Returns
/// `None` when no claim passes the hard gate.
///
/// Deliberately claim-score-only (no operational adjustments): this
/// picks among LOCAL models on one node for an already-admitted
/// request — load/locality/cold-start/availability are peer-shaped
/// signals that don't differentiate candidates sharing one host.
fn route_with_oicp(state: &AppState, req: &InferenceRequirements) -> Option<ModelId> {
    let models = state.inner.inference_store.list_models();
    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    let mut best_model = None;
    let mut best_name = String::new();
    let mut best_score = f32::NEG_INFINITY;

    for shard_plan in &plan.model_plans {
        let Some(model_info) = models.get(&shard_plan.model) else {
            continue;
        };
        for claim in synthesize_claims_for_model_info(model_info) {
            let score = oicp::score_claim_for_request(&claim, req);
            tracing::debug!(
                model = %model_info.name,
                hint = %claim.hint,
                latency_class = ?claim.latency_class,
                affinity = claim.affinity,
                score = score.unwrap_or(f32::NEG_INFINITY),
                gated = score.is_none(),
                "route_with_oicp: scored candidate"
            );
            if let Some(score) = score {
                if score > best_score {
                    best_score = score;
                    best_model = Some(shard_plan.model);
                    best_name = model_info.name.clone();
                }
            }
        }
    }

    if let Some(model) = best_model {
        tracing::info!(
            model = %best_name,
            score = best_score,
            req_hint = %req.effective_hint(),
            req_latency = ?req.effective_latency_class(),
            "route_with_oicp: selected"
        );
        Some(model)
    } else {
        tracing::info!(
            req_hint = %req.effective_hint(),
            req_latency = ?req.effective_latency_class(),
            "route_with_oicp: no claim passed the hard gate"
        );
        None
    }
}

/// Synthesize the v0.3 claims for a loaded `ModelInfo`. ONE
/// synthesis — `routes_oicp::synthesize_default_claims` — feeds both
/// the advertiser (the `/oicp/v1/capabilities` manifest) and this
/// scheduler, so they cannot drift. (Pre-2026-06-10 this was a
/// hand-maintained mirror, and single-claim: a small model could
/// never match a latency_class=Fast request here.)
fn synthesize_claims_for_model_info(
    model_info: &commonwealth_inference::ModelInfo,
) -> Vec<CapabilityClaim> {
    crate::routes_oicp::synthesize_default_claims(
        &model_info.name,
        &model_info.oicp_capabilities,
        32_768,
        model_info.size_bytes,
    )
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
                    .unwrap_or_default(),
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
                    .unwrap_or_default(),
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
                    .unwrap_or_default(),
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
    headers: HeaderMap,
    Json(request): Json<EmbeddingRequest>,
) -> Response {
    // Who is this for? A peer with no embed model of its own (driving
    // ingestion via `http_embed_fn`) carries `X-Node-Id`; a local
    // OpenAI-API client does not. Either way it's real embedding work
    // this daemon performed — recorded on the Activity ledger below.
    let requester = crate::headers::parse_x_node_id(&headers);
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
                .unwrap_or_default(),
            ),
        )
            .into_response();
    };

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
                .unwrap_or_default(),
            ),
        )
            .into_response();
    }

    let n_texts = inputs.len() as u64;
    let total_chars: usize = inputs.iter().map(|t| t.len()).sum();

    // One batch call: a single multi-sequence decode on the embedded engine,
    // or sharded across compute-child replicas by the routing facade. Both
    // beat the former per-input sequential loop for bulk ingest.
    let embeddings = match service.embed_batch(&inputs).await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "embeddings: local embed_batch failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        format!("embedding batch failed: {e}"),
                        "backend_error",
                    ))
                    .unwrap_or_default(),
                ),
            )
                .into_response();
        }
    };
    if embeddings.len() != inputs.len() {
        warn!(
            got = embeddings.len(),
            want = inputs.len(),
            "embeddings: backend returned the wrong number of vectors"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::to_value(ErrorResponse::new(
                    format!(
                        "embedding backend returned {} vectors for {} inputs",
                        embeddings.len(),
                        inputs.len()
                    ),
                    "backend_error",
                ))
                .unwrap_or_default(),
            ),
        )
            .into_response();
    }
    let data: Vec<EmbeddingData> = embeddings
        .into_iter()
        .enumerate()
        .map(|(i, embedding)| EmbeddingData {
            object: "embedding".into(),
            embedding,
            index: i,
        })
        .collect();

    // The OpenAI spec counts token usage; we only have char count, so
    // we produce a conservative ~4 chars/token estimate rather than
    // leaving the field out (some clients require it to be present).
    let approx_tokens = total_chars.div_ceil(4) as u32;
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
    // Record the embedding work on the local Activity ledger — split
    // peer (mesh-driven ingestion) vs local (own API client). This was
    // previously invisible: nothing recorded embeddings served.
    state
        .inner
        .activity_emitter
        .record(ActivityEventKind::EmbeddingsServed {
            served_for: match requester {
                Some(node_id) => ServedFor::Peer { node_id },
                None => ServedFor::Local,
            },
            n_texts,
            tokens: approx_tokens as u64,
        });
    (StatusCode::OK, Json(resp)).into_response()
}

/// GET /v1/models — the names this daemon can dispatch by name right now.
///
/// **The contract is dispatchability**, and it is testable: every id
/// returned here resolves through the same name resolution
/// `/v1/chat/completions` runs (`MeshInferenceProvider::locate_named_model`).
/// `models_endpoint_lists_only_dispatchable_names` is the gate.
///
/// ## Why this reads manifests and not the store
///
/// Until 2026-08-27 this scanned `inference_store` — the gossiped KV — and
/// kept an entry when *the node that last wrote it* was online. That tests
/// the wrong proposition. An online peer that never loaded (or has since
/// unloaded) a model still vouches for its store entry, so the endpoint
/// advertised ids that a chat completion refused with:
///
/// > no node in this mesh advertises model 'X' — check `/v1/models` for
/// > available names
///
/// The refusal pointed at the list that was wrong. Measured on a live
/// 2-node mesh: 12 entries returned, 6 in the manifest, one name listed
/// twice, two ids provably undispatchable. The cause was two registries
/// behind one question — the KV store here, live `ProviderManifest`s in the
/// resolver — with nothing reconciling them (ARCH §10.6, and §18.3: the
/// store path *defaulted* absent availability to present).
///
/// So the list is now built from the resolver's own inputs: this node's
/// manifest plus [`LocalInferenceService::peer_manifests`], which contracts
/// to return the same peers, under the same quarantine and cache rules,
/// that `locate_named_model` consults.
///
/// The store path survives as [`store_rows`] for the orchestrator daemon,
/// which has no embedded engine and therefore no manifest to read. It is a
/// strictly narrower claim than it used to make — see its own docs.
pub async fn list_models(
    State(state): State<AppState>,
    guest: Option<axum::Extension<crate::client_auth::Guest>>,
) -> impl IntoResponse {
    let mut data = match manifest_rows(&state).await {
        Some(rows) => rows,
        None => store_rows(&state).await,
    };
    // A guest sees only what their grant covers. This is the SAME contract the
    // rest of this handler keeps — every id returned is dispatchable — held for
    // one caller instead of for the node. Listing a name a guest would be
    // refused reintroduces exactly the defect this endpoint was rewritten to
    // remove, in a new place: the list would advertise, and the request would
    // refuse, and the refusal would point back at the list.
    if let Some(axum::Extension(crate::client_auth::Guest(grant))) = guest.as_ref() {
        data.retain(|m| grant.allows_model(&m.id));
    }
    // Stable order, and the dedup key is the id a caller would actually
    // send. Deterministic output matters for the Ollama `/api/tags` shim
    // and for anyone diffing the list across polls.
    data.sort_by(|a, b| a.id.cmp(&b.id));
    Json(ModelListResponse {
        object: "list".into(),
        data,
    })
}

/// Build the list from the manifests name resolution reads. `None` when
/// this node has no local inference service (the orchestrator daemon), so
/// the caller falls back to the store.
///
/// One row per distinct id, holders unioned. Two nodes advertising the same
/// model are ONE dispatchable name — the old store path emitted two rows
/// (observed: `Qwen3.8-27B-UD-Q6_K_XL` twice) because its key was
/// `hash(role, absolute path)`, which differs per machine for the same
/// weights. Grouping on the id a caller sends fixes that without touching
/// the `ModelId` scheme.
/// Every model id this node can dispatch by name right now, as
/// `/v1/models` would report it to an ungated caller.
///
/// Exists so the guest-grant mint route can refuse an unknown `--model`
/// against the SAME set the request path will resolve against. Re-deriving
/// that set at the mint site would be a second answer to "what can this node
/// serve" (§10.6) — and the failure would be quiet: a grant minted for a name
/// nothing advertises produces a link that looks fine and 403s on first use.
pub(crate) async fn dispatchable_ids(state: &AppState) -> Vec<String> {
    match manifest_rows(state).await {
        Some(rows) => rows,
        None => store_rows(state).await,
    }
    .into_iter()
    .map(|m| m.id)
    .collect()
}

/// `None` means "this node has no manifest surface at all", which is the
/// only condition that licenses the store fallback. An EMPTY manifest is
/// `Some(vec![])`, not `None`: a node advertising nothing can dispatch
/// nothing, and answering that with a list of store entries is precisely
/// the substitution this change removes (ARCH §18.3 — report the absence).
async fn manifest_rows(state: &AppState) -> Option<Vec<ModelObject>> {
    let service = state.inner.local_inference.as_ref()?;
    let local = service.provider_manifest()?;

    // (holder display name, model). Local first so it wins the
    // first-writer fields (claims, alias target) on a tie.
    let mut holders: Vec<(String, commonwealth_inference::oicp::ProviderModel)> = local
        .models
        .into_iter()
        .map(|m| (LOCAL_HOLDER.to_string(), m))
        .collect();
    // Peers in name order, so `advertised_by` reads the same across polls.
    // `peer_inference_endpoints` orders by whatever the roster yields, which
    // is not stable across gossip rounds, and a listing that reshuffles
    // itself is one nobody can diff.
    let mut peers = service.peer_manifests().await;
    peers.sort_by(|a, b| a.0.cmp(&b.0));
    for (peer_name, manifest) in peers {
        for model in manifest.models {
            holders.push((peer_name.clone(), model));
        }
    }

    // Models reachable through a GUEST LINK this node accepted. They belong
    // here for the reason peers do: `locate_named_model` routes these ids, so
    // a listing that omitted them would lie by omission — the same §10.6
    // failure the peer listing was fixed for, in the other direction. The
    // holder is the LENDER's own display name, never `LOCAL_HOLDER` and never
    // a peer name: `advertised_by` must not claim a mesh relationship that
    // does not exist.
    //
    // `Cold` on purpose (the default `ModelStatus`). A grant advertises no
    // residency and the lender publishes no slot state to a guest, so
    // claiming `Resident` would be a guess in the flattering direction. Cold
    // is honest and costs only a slower first turn.
    if let Some((lender, ids)) = service.lender_manifest().await {
        for id in ids {
            holders.push((
                lender.clone(),
                commonwealth_inference::oicp::ProviderModel {
                    id,
                    base_model: None,
                    quantization: None,
                    context_tokens: 0,
                    // available, NOT loaded: the grant says it can be
                    // dispatched, and nothing about whether the lender has
                    // the weights warm. Claiming `loaded` would upgrade the
                    // row to Resident on a guess.
                    status: commonwealth_inference::oicp::ModelStatus {
                        available: true,
                        loaded: false,
                        estimated_tokens_per_sec: None,
                        estimated_ttft_ms: None,
                        estimated_load_time_sec: None,
                    },
                    size_gb: None,
                    claims: Vec::new(),
                    fingerprint: None,
                },
            ));
        }
    }

    let slot_aliases = state.inner.slot_aliases.load();
    let mut rows: Vec<ModelObject> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (holder, model) in holders {
        let resident = model.status.loaded;
        if let Some(&row) = index.get(&model.id) {
            let existing: &mut ModelObject = &mut rows[row];
            if !existing.advertised_by.contains(&holder) {
                existing.advertised_by.push(holder);
            }
            // ANY holder with the weights in memory makes the name warm:
            // the resolver load-balances across holders and will pick one.
            // A cold row upgrading to Resident is the honest direction; the
            // reverse would let one cold peer mask a warm local slot.
            if resident {
                existing.residency = Some(Residency::Resident);
                if let Some(perf) = existing.performance.as_mut() {
                    perf.loaded = true;
                }
            }
            continue;
        }

        let residency = if resident {
            Residency::Resident
        } else {
            Residency::Cold
        };
        index.insert(model.id.clone(), rows.len());
        rows.push(ModelObject {
            // An alias (`primary`, `commonwealth/fast`) is a first-class
            // dispatchable name, not a synthetic decoration: it appears here
            // because a manifest advertised it, so it is resolvable by
            // definition. The pre-2026-08-27 handler appended aliases from
            // `slot_aliases` unconditionally, which is how `embed` came to be
            // listed on a node whose manifest never advertised it.
            //
            // The target named here is THIS node's binding. That is the right
            // one to show even on a row a peer also advertises: an alias is
            // dereferenced by whichever node ends up serving, so "what does
            // `primary` resolve to" is node-relative by design, and this is
            // the answer that applies if the request stays here.
            owned_by: match slot_aliases.get(&model.id) {
                Some(target) => format!("alias→{target}"),
                None => "mesh".into(),
            },
            id: model.id,
            object: "model".into(),
            created: 0,
            // The manifest's capability CLAIMS, which is what the scheduler
            // actually scores. The store path published a `CapabilityProfile`
            // here instead — a different shape for the same field, and the
            // one further from the routing decision.
            capabilities: serde_json::to_value(&model.claims).ok(),
            performance: Some(ModelPerformance {
                // The manifest carries per-claim throughput, not a per-model
                // estimate; the orchestrator's shard plan was the only source
                // of these and it does not exist on the embedded path. Zeroed
                // rather than omitted so `loaded` stays readable — absence
                // here is what made availability invisible before.
                estimated_tokens_per_sec: 0.0,
                estimated_ttft_ms: 0,
                loaded: resident,
            }),
            residency: Some(residency),
            advertised_by: vec![holder],
        });
    }

    Some(rows)
}

/// The pre-2026-08-27 store scan, kept for the orchestrator daemon — the
/// topology with no embedded engine, where llama-servers are spawned per
/// model and `inference_store` IS the local record of what was scheduled.
///
/// **This path cannot promise dispatchability**, only that the entry's last
/// writer is reachable. It is retained because on the orchestrator there is
/// no manifest to consult and a narrower list would be empty; it is not the
/// path any mesh node with local inference takes. Deduped by name like the
/// manifest path, so the duplicate-row bug is fixed on both.
async fn store_rows(state: &AppState) -> Vec<ModelObject> {
    let local_id = state.self_node_id();
    let live_nodes: HashSet<NodeId> = {
        let mesh = state.inner.mesh.read().await;
        std::iter::once(local_id)
            .chain(
                mesh.members
                    .values()
                    .filter(|m| matches!(m.status, NodeStatus::Online | NodeStatus::Busy))
                    .map(|m| m.node_id),
            )
            .collect()
    };

    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    // Ground-truth residency from the embedded engine (empty on the
    // orchestrator daemon). Used only to OR-correct the `loaded` flag,
    // which the `llama_addr:` store key can't answer on the embedded
    // path — never removes the orchestrator signal.
    let resident: Vec<crate::state::ResidentSlot> = match &state.inner.local_inference {
        Some(svc) => svc.resident_slots(),
        None => Vec::new(),
    };

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
                .is_some()
                || resident
                    .iter()
                    .any(|r| r.resident && r.model_id == model.name);

            ModelObject {
                id: model.name.clone(),
                object: "model".into(),
                created: 0,
                owned_by: "mesh".into(),
                capabilities: Some(
                    serde_json::to_value(&model.oicp_capabilities).unwrap_or_default(),
                ),
                performance: match shard_plan {
                    Some(p) => Some(ModelPerformance {
                        estimated_tokens_per_sec: p.estimated_tokens_per_sec,
                        estimated_ttft_ms: p.estimated_ttft_ms,
                        loaded,
                    }),
                    // The embedded topology has no shard plan, so without this
                    // a resident model would carry no `performance` block and
                    // its `loaded` truth (plus Ollama's `/api/ps`, which filters
                    // on it) would be invisible. Surface residency with
                    // zeroed estimates when the plan can't provide them.
                    None if loaded => Some(ModelPerformance {
                        estimated_tokens_per_sec: 0.0,
                        estimated_ttft_ms: 0,
                        loaded: true,
                    }),
                    None => None,
                },
                residency: Some(if loaded {
                    Residency::Resident
                } else {
                    Residency::Cold
                }),
                // The store cannot answer this. Its key is the model, not
                // (node, model), and `ModelInfo::available_on` — the field
                // built to hold it — is unpopulated at every construction
                // site and unpopulat*able* (`NodeId` serialises as a byte
                // array, so a `HashMap<NodeId, _>` fails to round-trip and
                // entries vanish from this very endpoint). Reporting the
                // absence rather than inventing a holder (ARCH §18.3).
                advertised_by: Vec::new(),
            }
        })
        .collect();

    // One row per NAME. The store keys on `hash(role, absolute path)`, so
    // the same GGUF on two machines is two entries with one name, and a
    // re-registration under a second role duplicates it on one machine.
    // Both showed up live as a doubled `Qwen3.8-27B-UD-Q6_K_XL`. Callers
    // dispatch by name, so the name is the identity here (ARCH §7.5).
    let mut seen: HashSet<String> = HashSet::new();
    data.retain(|m| seen.insert(m.id.clone()));

    // Append an entry for each registered slot alias so discovery surfaces
    // (opencode's model picker, `/v1/models` curl) see the slot names
    // alongside the GGUF stems. Both the bare and `commonwealth/`-namespaced
    // forms are listed: opencode treats them as separate ids and we want
    // either spelling to be findable. Dereferenced server-side at request
    // time — no client config churn when an operator swaps GGUFs.
    //
    // Only the ORCHESTRATOR needs this. On the embedded path the aliases
    // are advertised by the manifest itself, which is what makes them
    // dispatchable; synthesising them from `slot_aliases` there is how the
    // `embed` alias came to be listed on a node whose manifest never
    // carried it, permanently un-dispatchable.
    let slot_aliases = state.inner.slot_aliases.load();
    let mut alias_entries: Vec<(String, String)> = slot_aliases
        .iter()
        .map(|(alias, target)| (alias.clone(), target.clone()))
        .collect();
    alias_entries.sort();
    for (alias, target) in alias_entries {
        if !seen.insert(alias.clone()) {
            continue;
        }
        // An alias is exactly as warm as the slot behind it — resolve
        // through the target rather than reporting the alias as unknown.
        let residency = data
            .iter()
            .find(|m| m.id == target)
            .and_then(|m| m.residency);
        data.push(ModelObject {
            id: alias,
            object: "model".into(),
            created: 0,
            owned_by: format!("alias→{target}"),
            capabilities: None,
            performance: None,
            residency,
            advertised_by: Vec::new(),
        });
    }

    data
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
    // Snapshot the path-component frequencies BEFORE moving `request`
    // into the backend call. The post-emission path canonicalizer
    // uses this map to detect tokenizer-drift typos: when an emitted
    // path component is similar to a more-frequent component in
    // context, rewrite to the canonical (frequent) form.
    let context_components = crate::frontdoor::gather_context_components(&request.messages);
    let started = Instant::now();
    match service.chat_completion(request).await {
        Ok(mut resp) => {
            // Post-generation canonicalization, two passes:
            //   1. Heredoc canonicalizer — repairs malformed
            //      apply_patch shapes (extra `***`, missing line
            //      breaks). No-op for non-heredoc cmds.
            //   2. Path canonicalizer — rewrites tokenizer-drifted
            //      absolute paths to their canonical form when a
            //      similar path appears in the request context.
            //      No-op when emitted path is already canonical OR
            //      no similar path exists.
            for choice in resp.choices.iter_mut() {
                // First pass: promote in-content tool calls into the
                // structured `tool_calls` field. Some models (Qwen3
                // family observed in the search-gym harness) emit
                // tool calls as JSON objects in the message content
                // rather than the structured channel. Without this
                // promotion, downstream consumers see tool_calls=[]
                // and the tool intent is silently lost.
                if crate::frontdoor::promote_in_content_tool_call(&mut choice.message) {
                    info!("promoted in-content tool call into structured tool_calls field");
                }

                // Both canonicalizers REWRITE arguments the model
                // already emitted, so they answer to the same opt-out
                // as the request-side nudges. The promotion above does
                // not: it recovers a call that would otherwise be lost.
                if !crate::turn_fidelity::reshape_enabled() {
                    continue;
                }

                if let Some(tcs) = choice.message.tool_calls.as_mut() {
                    let heredoc_fixed =
                        crate::frontdoor::canonicalize_chat_response_tool_calls(tcs);
                    if heredoc_fixed > 0 {
                        debug!(
                            count = heredoc_fixed,
                            "apply_patch heredoc canonicalized in tool_calls"
                        );
                    }
                    let path_fixed = crate::frontdoor::canonicalize_chat_response_paths(
                        tcs,
                        &context_components,
                    );
                    if path_fixed > 0 {
                        info!(
                            count = path_fixed,
                            "absolute paths canonicalized in tool_calls"
                        );
                    }
                }
            }
            if let Some(for_node) = requester {
                let tokens = resp
                    .usage
                    .as_ref()
                    .map(|u| u.completion_tokens as u64)
                    .unwrap_or(0);
                let wall_seconds = started.elapsed().as_secs_f64();
                state
                    .inner
                    .contribution_emitter
                    .record(LedgerEventKind::InferenceServed {
                        for_node,
                        model_id,
                        tokens_generated: tokens,
                        wall_seconds,
                    });
            } else {
                // Local API client (no `X-Node-Id`). The contribution
                // ledger deliberately skips this — it's not work *for
                // the mesh* — but it IS resource work this daemon did,
                // so record it on the local Activity ledger. Without
                // this, a solo user's own OpenAI-API traffic through
                // the daemon would be invisible in the glassbox view.
                let (prompt_tokens, completion_tokens) = resp
                    .usage
                    .as_ref()
                    .map(|u| (u.prompt_tokens as u64, u.completion_tokens as u64))
                    .unwrap_or((0, 0));
                let wall_seconds = started.elapsed().as_secs_f64();
                state
                    .inner
                    .activity_emitter
                    .record(ActivityEventKind::LocalInferenceServed {
                        model_id,
                        prompt_tokens,
                        completion_tokens,
                        wall_seconds,
                    });
            }
            (StatusCode::OK, Json(resp)).into_response()
        }
        // A shed is backpressure, not a fault: it must carry
        // `Retry-After` so a client can tell "busy, come back" from
        // "this broke". See admission::shed_response.
        Err(crate::state::LocalInferenceError::Shed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        }) => {
            warn!(
                queue_position = position,
                predicted_wait_ms, retry_after_secs, "chat_completions: local queue shed"
            );
            crate::admission::local_queue_shed_response(
                position,
                predicted_wait_ms,
                retry_after_secs,
            )
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
                    .unwrap_or_default(),
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
        // Same rule on the streaming path — and this is the one most
        // clients actually hit, so a shed rendered as `backend_error`
        // here is what makes backpressure look like a crash.
        Err(crate::state::LocalInferenceError::Shed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        }) => {
            warn!(
                queue_position = position,
                predicted_wait_ms,
                retry_after_secs,
                "chat_completions: local queue shed before stream start"
            );
            return crate::admission::local_queue_shed_response(
                position,
                predicted_wait_ms,
                retry_after_secs,
            );
        }
        Err(e) => {
            warn!(error = %e, "chat_completions: local stream failed to start");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::to_value(ErrorResponse::new(
                        format!("local stream failed: {e}"),
                        "backend_error",
                    ))
                    .unwrap_or_default(),
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
                chunks_count_for_stream.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                Ok::<_, std::convert::Infallible>(Event::default().data(chunk.to_string()))
            }
            StreamFrame::ToolCalls(calls) => {
                // Synthetic tools-streaming chunk. Local backends
                // parse `<tool_call>` markup post-generation, so we
                // emit one chunk carrying every parsed call rather
                // than the per-fragment `arguments` deltas the
                // OpenAI spec also permits. Both shapes are
                // wire-legal — clients accumulate by `tool_calls[i].
                // index` regardless of chunk count.
                chunks_count_for_stream.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let tool_calls_json: Vec<serde_json::Value> = calls
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        serde_json::json!({
                            "index": i,
                            "id": c.id,
                            "type": c.kind,
                            "function": {
                                "name": c.function.name,
                                "arguments": c.function.arguments,
                            }
                        })
                    })
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
                Ok::<_, std::convert::Infallible>(Event::default().data(chunk.to_string()))
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
            StreamFrame::Debug(_) => {
                // FIM-only glassbox frame; the chat path never
                // produces it. Drop defensively so a future producer
                // can't leak internals onto an unrelated surface.
                Ok(Event::default().comment("debug frame dropped"))
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
    let activity_for_done = state.inner.activity_emitter.clone();
    let requester_for_done = requester;
    let model_for_done = model_id_for_ledger;
    let done = futures::stream::once(async move {
        let tokens = chunks_for_done.load(std::sync::atomic::Ordering::Relaxed);
        let wall_seconds = started.elapsed().as_secs_f64();
        if let Some(for_node) = requester_for_done {
            state_for_done.record(LedgerEventKind::InferenceServed {
                for_node,
                model_id: model_for_done,
                tokens_generated: tokens,
                wall_seconds,
            });
        } else {
            // Local API client — record on the Activity ledger, same
            // as the non-streaming path. Stream frames ≈ completion
            // tokens; prompt token count isn't available on this path.
            activity_for_done.record(ActivityEventKind::LocalInferenceServed {
                model_id: model_for_done,
                prompt_tokens: 0,
                completion_tokens: tokens,
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
#[cfg(feature = "atos")]
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
        .unwrap_or_else(uuid_like_for_sessionless);

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
#[cfg(feature = "atos")]
pub(crate) struct PostPathGuard {
    pipeline: std::sync::Arc<Pipeline>,
    ctx: std::sync::Arc<PipelineContext>,
    session_store: sovereign_atos::session::SessionStore,
    session_id: String,
}

#[cfg(feature = "atos")]
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
#[cfg(feature = "atos")]
fn uuid_like_for_sessionless() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sessionless-{nanos:x}")
}

#[cfg(feature = "atos")]
fn middleware_error_to_response(err: MiddlewareError) -> Response {
    match err {
        MiddlewareError::ApprovalRequired { feature_id, hint } => atos_error_response(
            StatusCode::FORBIDDEN,
            "atos_approval_required",
            &format!("feature '{feature_id}' is not approved: {hint}"),
        ),
        MiddlewareError::PipelineRejected(msg) => {
            atos_error_response(StatusCode::FORBIDDEN, "atos_pipeline_rejected", &msg)
        }
        MiddlewareError::Infra(msg) => atos_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "atos_pipeline_infra_error",
            &msg,
        ),
    }
}

#[cfg(feature = "atos")]
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

#[cfg(test)]
mod shed_rendering_tests {
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
        fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
            None
        }
        async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
            unimplemented!("embedding is not on the shed path")
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
}

#[cfg(test)]
mod list_models_tests {
    //! `/v1/models` promises DISPATCHABILITY. These pin that promise to the
    //! one thing that can keep it: the list is a function of the manifests
    //! name resolution reads, and of nothing else.
    //!
    //! The failure they encode was measured on a live 2-node mesh
    //! (2026-08-27), not imagined. `/v1/models` returned 12 entries against
    //! 6 in the capability manifest, listed `Qwen3.8-27B-UD-Q6_K_XL` twice,
    //! and advertised `Qwen3-Embedding-0.6B-Q8_0`, which chat completions
    //! refused with "no node in this mesh advertises model
    //! 'Qwen3-Embedding-0.6B-Q8_0' — check `/v1/models` for available
    //! names". The refusal named the list that was wrong.
    //!
    //! ## Which of these actually has teeth
    //!
    //! Measured, by forcing `manifest_rows` to return `None` and watching:
    //! FOUR go red — `a_store_entry_no_manifest_carries_is_not_listed`,
    //! `one_name_held_by_two_nodes_is_one_row_naming_both`,
    //! `a_name_is_resident_when_any_holder_has_it_resident`,
    //! `a_held_but_unloaded_model_lists_as_cold_not_missing`.
    //!
    //! `every_listed_id_is_advertised_by_some_manifest` does NOT, and it
    //! reads like the headline gate, so say so plainly: it passes
    //! vacuously whenever the list is empty, which is what the store path
    //! produces in this fixture. It states the contract; it does not
    //! defend it. **The load-bearing one is
    //! `a_store_entry_no_manifest_carries_is_not_listed`** — it fails
    //! exactly when a non-manifest source gets back into the listing,
    //! which is the whole regression class. Reach for that one first if
    //! you are changing this handler.

    use super::*;
    use crate::state::{test_app_state, LocalInferenceError, LocalInferenceService};
    use commonwealth_inference::oicp::{ModelStatus, ProviderManifest, ProviderModel};
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::Arc;

    fn model(id: &str, loaded: bool) -> ProviderModel {
        ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: None,
            claims: vec![],
            fingerprint: None,
        }
    }

    /// A node whose manifest carries `local`, and whose one reachable peer
    /// carries `shared` (which it also holds, cold) plus `peer-only`.
    struct TwoNodeMesh;

    #[async_trait::async_trait]
    impl LocalInferenceService for TwoNodeMesh {
        /// Echoes back the model it was asked to serve.
        ///
        /// It used to `unimplemented!()` — listing does not generate. The
        /// guest tests need the ADMITTED arm of the scope gate to be
        /// observable, and "the request reached dispatch" is only observable
        /// if dispatch answers. Echoing the model id also makes a silent
        /// substitution visible: if the gate ever let a request through and
        /// something downstream swapped the name, this response says so.
        async fn chat_completion(
            &self,
            r: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, LocalInferenceError> {
            Ok(ChatCompletionResponse {
                id: "test".into(),
                object: "chat.completion".into(),
                created: 0,
                model: r.model.unwrap_or_default(),
                choices: vec![],
                usage: None,
            })
        }
        async fn chat_completion_stream(
            &self,
            _r: ChatCompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, LocalInferenceError> {
            unimplemented!("listing does not generate")
        }
        async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
            unimplemented!("listing does not embed")
        }
        fn provider_manifest(&self) -> Option<ProviderManifest> {
            Some(ProviderManifest::new(vec![
                model("local-fast", true),
                // Held here, idle-unloaded. The lazy primary's steady state.
                model("shared-primary", false),
            ]))
        }
        async fn peer_manifests(&self) -> Vec<(String, ProviderManifest)> {
            vec![(
                "RuggedFox".into(),
                ProviderManifest::new(vec![
                    // Same name, other machine, and WARM there.
                    model("shared-primary", true),
                    model("peer-only", true),
                ]),
            )]
        }
    }

    async fn rows(service: Arc<dyn LocalInferenceService>) -> Vec<ModelObject> {
        let state = test_app_state().with_local_inference(service);
        let resp = list_models(State(state), None).await.into_response();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body reads");
        serde_json::from_slice::<ModelListResponse>(&body)
            .expect("body is a model list")
            .data
    }

    /// THE gate. Every id returned must come from a manifest, because a
    /// manifest is what `locate_named_model` resolves against — so an id
    /// here is an id that dispatches. An entry sourced from anywhere else
    /// (the gossiped KV store, a synthesised alias) is the regression.
    #[tokio::test]
    async fn every_listed_id_is_advertised_by_some_manifest() {
        let advertised: HashSet<String> = ["local-fast", "shared-primary", "peer-only"]
            .into_iter()
            .map(String::from)
            .collect();
        for row in rows(Arc::new(TwoNodeMesh)).await {
            assert!(
                advertised.contains(&row.id),
                "'{}' is listed but no manifest advertises it — a chat \
                 completion naming it would be refused with 'no node in this \
                 mesh advertises model', pointing the operator back at this list",
                row.id
            );
        }
    }

    /// The KV store is no longer an input. Registering a model there — the
    /// only thing the pre-fix handler read — must not put it on the list.
    #[tokio::test]
    async fn a_store_entry_no_manifest_carries_is_not_listed() {
        let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
        state.register_model(commonwealth_inference::ModelInfo {
            id: commonwealth_core::ModelId::from_u128(7),
            name: "ghost-from-gossip".into(),
            repo: String::new(),
            file: "ghost.gguf".into(),
            size_bytes: 1,
            total_layers: 0,
            architecture: commonwealth_inference::model::ModelArchitecture::Other,
            available_on: std::collections::HashMap::new(),
            oicp_capabilities: Default::default(),
            quantization: String::new(),
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        });

        let resp = list_models(State(state), None).await.into_response();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: ModelListResponse = serde_json::from_slice(&body).unwrap();
        assert!(
            !list.data.iter().any(|m| m.id == "ghost-from-gossip"),
            "a gossiped store entry is not evidence that anything can serve it"
        );
    }

    /// One name, one row, both holders. The store keyed on
    /// `hash(role, absolute path)`, so the same weights on two machines were
    /// two entries with one name — which is what put the 27B on the live
    /// list twice.
    #[tokio::test]
    async fn one_name_held_by_two_nodes_is_one_row_naming_both() {
        let rows = rows(Arc::new(TwoNodeMesh)).await;
        let shared: Vec<&ModelObject> = rows.iter().filter(|m| m.id == "shared-primary").collect();
        assert_eq!(shared.len(), 1, "one dispatchable name is one row");
        assert_eq!(
            shared[0].advertised_by,
            vec!["local".to_string(), "RuggedFox".to_string()],
            "both holders are named — `owned_by: \"mesh\"` could not say this"
        );
    }

    /// Cold here, warm on the peer: the name is warm, because the resolver
    /// load-balances across holders and will pick the peer. The reverse
    /// reading would let one idle-unloaded node mask a warm mesh.
    #[tokio::test]
    async fn a_name_is_resident_when_any_holder_has_it_resident() {
        let rows = rows(Arc::new(TwoNodeMesh)).await;
        let by_id = |id: &str| -> ModelObject {
            rows.iter().find(|m| m.id == id).cloned().expect("listed")
        };
        assert_eq!(by_id("shared-primary").residency, Some(Residency::Resident));
        assert_eq!(by_id("local-fast").residency, Some(Residency::Resident));
        assert_eq!(
            by_id("shared-primary")
                .performance
                .expect("performance block is always present on this path")
                .loaded,
            true,
            "the legacy flag agrees with the new field rather than contradicting it"
        );
    }

    /// A node with weights nobody has loaded is still dispatchable — the
    /// first request pays a cold load. That is normal operation for a lazy
    /// primary and must not read as unavailable.
    #[tokio::test]
    async fn a_held_but_unloaded_model_lists_as_cold_not_missing() {
        struct ColdOnly;
        #[async_trait::async_trait]
        impl LocalInferenceService for ColdOnly {
            async fn chat_completion(
                &self,
                _r: ChatCompletionRequest,
            ) -> Result<ChatCompletionResponse, LocalInferenceError> {
                unimplemented!()
            }
            async fn chat_completion_stream(
                &self,
                _r: ChatCompletionRequest,
            ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, LocalInferenceError>
            {
                unimplemented!()
            }
            async fn embed(&self, _i: &str) -> Result<Vec<f32>, String> {
                unimplemented!()
            }
            fn provider_manifest(&self) -> Option<ProviderManifest> {
                Some(ProviderManifest::new(vec![model("big-primary", false)]))
            }
        }
        let rows = rows(Arc::new(ColdOnly)).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "big-primary");
        assert_eq!(rows[0].residency, Some(Residency::Cold));
    }

    /// A provider with no manifest (the orchestrator daemon) falls back to
    /// the store rather than returning nothing. The fallback is narrower
    /// than the manifest path and says so in its docs; what it must not do
    /// is disappear.
    #[tokio::test]
    async fn no_local_inference_falls_back_to_the_store() {
        let state = test_app_state();
        state.register_model(commonwealth_inference::ModelInfo {
            id: commonwealth_core::ModelId::from_u128(9),
            name: "orchestrated".into(),
            repo: String::new(),
            file: "o.gguf".into(),
            size_bytes: 1,
            total_layers: 0,
            architecture: commonwealth_inference::model::ModelArchitecture::Other,
            available_on: std::collections::HashMap::new(),
            oicp_capabilities: Default::default(),
            quantization: String::new(),
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        });
        let resp = list_models(State(state), None).await.into_response();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let list: ModelListResponse = serde_json::from_slice(&body).unwrap();
        assert!(list.data.iter().any(|m| m.id == "orchestrated"));
    }

    // ── guest scope refinement ───────────────────────────────────────────
    //
    // `client_auth` decided this caller may reach the ROUTE. These pin the
    // half it cannot decide, because it lives in the body: WHICH MODEL.
    //
    // Both were watched fail — see `guest_falsification` in
    // `tests/client_auth.rs` for the probe and what went red.

    use commonwealth_knowledge::{GuestGrant, Scope};

    /// A live grant over `models`, as `client_auth` would have inserted it.
    fn guest_for(models: &[&str]) -> Option<axum::Extension<crate::client_auth::Guest>> {
        Some(axum::Extension(crate::client_auth::Guest(Arc::new(
            GuestGrant {
                token: "t".into(),
                scopes: vec![Scope::Models(
                    models.iter().map(|m| m.to_string()).collect(),
                )],
                label: None,
                issued_at_ms: 0,
                expires_at_ms: u64::MAX,
                revoked: false,
            },
        ))))
    }

    async fn chat_as_guest(
        model: Option<&str>,
        granted: &[&str],
    ) -> (StatusCode, serde_json::Value) {
        let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
        }))
        .expect("request shape");
        let resp = chat_completions(
            State(state),
            HeaderMap::new(),
            guest_for(granted),
            Json(request),
        )
        .await;
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// **THE §18.3 gate**, and the reason this refinement is in the handler
    /// rather than the auth layer. Without the refusal, an out-of-scope
    /// `model` walks down to Priority 4 and is served by `default_model_id()`:
    /// asked for one model, got another, HTTP 200, no way to tell. That is
    /// `d45489a3` verbatim — so the assertion is on the BODY, not the status.
    #[tokio::test]
    async fn a_guest_naming_an_ungranted_model_is_refused_not_served_the_default() {
        let (status, body) = chat_as_guest(Some("peer-only"), &["shared-primary"]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["type"], "guest_scope");
        assert_eq!(body["error"]["code"], "model_not_granted");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("peer-only") && message.contains("shared-primary"),
            "the refusal names what was asked AND what is granted: {message}"
        );
    }

    /// The subtler half. An absent `model` is exactly what reaches the default
    /// today — so "no model named" must refuse too, or the gate is bypassed by
    /// omitting a field rather than by naming the wrong one.
    #[tokio::test]
    async fn a_guest_naming_no_model_at_all_is_refused_rather_than_defaulted() {
        let (status, body) = chat_as_guest(None, &["shared-primary"]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "model_not_granted");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no model named"));
    }

    /// Whitespace is not a widening. `" shared-primary "` trims to the granted
    /// name; `"shared-primary-v2"` does not become it.
    #[tokio::test]
    async fn the_model_match_is_exact_after_trimming() {
        let (padded, _) = chat_as_guest(Some("  shared-primary  "), &["shared-primary"]).await;
        assert_ne!(
            padded,
            StatusCode::FORBIDDEN,
            "a trimmed exact name is inside the grant"
        );
        let (prefixed, _) = chat_as_guest(Some("shared-primary-v2"), &["shared-primary"]).await;
        assert_eq!(
            prefixed,
            StatusCode::FORBIDDEN,
            "a longer name that merely starts with a granted one is NOT granted"
        );
    }

    /// The ADMITTED arm, and the reason the refusal tests are not vacuous: a
    /// granted model reaches dispatch, and comes back as ITSELF. Without this
    /// the whole gate could be "refuse every guest" and every other test here
    /// would still pass.
    #[tokio::test]
    async fn a_guest_naming_a_granted_model_is_served_that_model() {
        let (status, body) = chat_as_guest(Some("shared-primary"), &["shared-primary"]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["model"], "shared-primary",
            "served the model that was asked for, not a default"
        );
    }

    /// The listing keeps the same contract for one caller that it keeps for
    /// the node: every id returned is dispatchable BY THEM. A guest shown a
    /// name they would be refused reintroduces the `/v1/models` defect in a
    /// new place — the list would advertise and the request would refuse, and
    /// the refusal would point back at the list.
    #[tokio::test]
    async fn a_guest_sees_only_the_models_its_grant_names() {
        let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
        let ungated = list_models(State(state.clone()), None)
            .await
            .into_response();
        let ungated: ModelListResponse = serde_json::from_slice(
            &axum::body::to_bytes(ungated.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        // The fixture advertises more than one name, or the filter below would
        // pass vacuously.
        assert!(
            ungated.data.len() > 1,
            "fixture must list several models for the filter to mean anything"
        );

        let gated = list_models(State(state), guest_for(&["shared-primary"]))
            .await
            .into_response();
        let gated: ModelListResponse = serde_json::from_slice(
            &axum::body::to_bytes(gated.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = gated.data.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["shared-primary"]);
    }

    /// A grant naming a model this node cannot serve lists NOTHING — it does
    /// not conjure a row from the grant. The mint route is what stops such a
    /// grant existing; this pins that the listing never papers over one.
    #[tokio::test]
    async fn a_grant_naming_an_unserved_model_lists_nothing() {
        let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
        let resp = list_models(State(state), guest_for(&["not-on-this-node"]))
            .await
            .into_response();
        let list: ModelListResponse = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(list.data.is_empty());
    }

    /// `dispatchable_ids` is what the mint route gates `--model` against. It
    /// must be the SAME set `/v1/models` reports to an ungated caller — a
    /// second answer here is how a grant gets minted for a name the request
    /// path will refuse (§10.6).
    #[tokio::test]
    async fn dispatchable_ids_matches_what_an_ungated_listing_reports() {
        let state = test_app_state().with_local_inference(Arc::new(TwoNodeMesh));
        let resp = list_models(State(state.clone()), None)
            .await
            .into_response();
        let listed: ModelListResponse = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let mut from_listing: Vec<String> = listed.data.iter().map(|m| m.id.clone()).collect();
        let mut from_mint_gate = dispatchable_ids(&state).await;
        from_listing.sort();
        from_mint_gate.sort();
        assert_eq!(from_mint_gate, from_listing);
        assert!(
            !from_mint_gate.is_empty(),
            "fixture must advertise something"
        );
    }
}
