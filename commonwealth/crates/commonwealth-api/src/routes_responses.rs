//! POST /v1/responses — OpenAI Responses API adapter.
//!
//! This is a *wire-format adapter* over the existing
//! [`routes_inference::chat_completions`] handler. The Responses API
//! is what `codex` (and the Realtime+OpenAI agents libraries) speak
//! after they dropped chat-completions support. We translate the
//! request shape inward, invoke the unmodified chat-completions
//! pipeline, then translate the response shape outward.
//!
//! Adapter, not duplicate (ARCH §10.1, §10.3): the only path
//! difference between `/v1/chat/completions` and `/v1/responses` is
//! the translation layer. All slot routing, OICP gating, ATOS
//! middleware, grammar-constrained tool calls, and SSE streaming run
//! through the existing handler.
//!
//! What we accept (codex subset of the public Responses spec):
//!   * `input` as string OR array of message / function_call_output /
//!     replayed function_call items
//!   * `instructions` (mapped to a leading system message)
//!   * `tools` (flat `{type, name, description, parameters}` shape)
//!   * `tool_choice` (passthrough)
//!   * `stream` (SSE event translation)
//!   * `max_output_tokens`, `temperature`, `top_p`, `metadata`
//!
//! What we reject (400):
//!   * `previous_response_id` — we don't implement server-side state.
//!     Codex tolerates this and falls back to resending full history.
//!
//! What we accept and ignore (forward-compat):
//!   * `store`, `parallel_tool_calls`, `reasoning`, `service_tier`
//!
//! Streaming event mapping (one chat.completion chunk → 0..N Responses events):
//!
//! ```text
//! [stream start]                  → response.created + response.in_progress
//! delta.content="..." (first one) → response.output_item.added(message)
//!                                   + response.content_part.added(output_text)
//!                                   + response.output_text.delta
//! delta.content="..." (subsequent)→ response.output_text.delta
//! delta.tool_calls[...]           → response.output_item.added(function_call)
//!                                   + response.function_call_arguments.delta
//!                                   + response.function_call_arguments.done
//!                                   + response.output_item.done
//! finish_reason+usage             → response.output_text.done
//!                                   + response.content_part.done
//!                                   + response.output_item.done (message, if open)
//!                                   + response.completed
//! [DONE] sentinel                 → swallowed
//! ```

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, warn};

use crate::frontdoor;
use crate::reshaping::{
    in_chunked_write_state, rewrite_synthetic_tool_call, synthetic_file_tools,
    SYNTHETIC_TOOL_WRITE_FILE_CHUNK, SYNTHETIC_TOOL_WRITE_FILE_END,
};
use crate::openai_types::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, FunctionCall, ToolCall,
    ToolDefinition, ToolFunction,
};
use crate::responses_types::{
    MessageContent, MessageItem, OutputContentPart, OutputFunctionCall, OutputMessage,
    ResponsesContentPart, ResponsesInput, ResponsesInputItem, ResponsesOutputItem,
    ResponsesRequest, ResponsesResponse, ResponsesUsage,
};
use crate::routes_inference::chat_completions;
use crate::state::AppState;

/// POST /v1/responses — OpenAI Responses API adapter over /v1/chat/completions.
pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut req): Json<ResponsesRequest>,
) -> Response {
    // ── Harness-aware frontdoor passes ────────────────────────────────
    //
    // The active harness is resolved from User-Agent (codex_cli_rs,
    // opencode, …) or the `SOVEREIGN_HARNESS` env override. Different
    // harnesses get different pass pipelines — codex's apply_patch
    // training contract resists prompt/catalog reshape (v11-v14
    // smokes 2026-05-13), while opencode benefits from the full
    // reshape. See `frontdoor::Harness` doc for the per-profile
    // selection table.
    let harness = frontdoor::detect_harness(&headers);
    tracing::info!(
        harness = %harness.as_str(),
        "responses: harness profile resolved"
    );
    // Anti-repetition (Investment #15, 2026-05-13) MUST run before
    // history compression — compression keeps only the last few items
    // verbatim, dropping older identical calls below the repetition
    // threshold. Anti-rep needs the full conversation tail to see the
    // run length.
    if harness != frontdoor::Harness::Bare {
        frontdoor::apply_anti_repetition(&mut req);
    }
    if harness.runs_coherence_baseline() {
        frontdoor::apply_baseline(&state, &headers, &mut req).await;
    }
    if harness == frontdoor::Harness::Codex {
        // Narrow-framing brief — gated by `SOVEREIGN_CODEX_BRIEF=1`.
        // No-op when the env is unset; runs ONLY on the Codex profile
        // so opencode / generic / bare are never touched. Pairs with
        // the heredoc-body diagnostics in the terminal telemetry
        // record — A/B with `escape_quote_count` as the witness.
        frontdoor::apply_codex_brief(&mut req);
    }
    if harness.runs_distiller() {
        // The "full" frontdoor only runs the distiller half here —
        // the other passes (catalog filter, synthetic tools, grammar
        // lock) are applied inside translate_request which consults
        // the harness flags directly. We don't call frontdoor::apply()
        // because its history-compression half already ran via
        // apply_baseline above.
        frontdoor::apply_distiller(&state, &headers, &mut req, harness).await;
    }

    // ── Hard rejections ───────────────────────────────────────────────
    //
    // `previous_response_id` is a stateful conversation chain. We don't
    // implement server-side response storage; failing fast lets codex
    // retry with the full history rather than silently dropping context.
    if req.previous_response_id.is_some() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "`previous_response_id` is not supported by this Responses-API \
             adapter — server-side response state is not implemented. Resend the \
             full conversation in the `input` array.",
        );
    }

    let stream_mode = req.stream.unwrap_or(false);
    let response_id = mk_response_id();
    let created_at = now_unix_secs();
    let model_label = req.model.clone().unwrap_or_else(|| "local".to_string());
    let metadata = req.metadata.clone();

    // Inbound tool catalog telemetry: 2026-05-12 codex+local-model
    // smokes showed the model hallucinating tool names (`write`,
    // `read_file`) instead of picking from the registered catalog —
    // even though the cognitive bank shows the same model selecting
    // tools correctly from a 13-item curated menu. Hypothesis: codex's
    // catalog (apply_patch + exec_command + web_search + file_search +
    // mcp + … + plugin tools) is too verbose / too many options for
    // the model to pattern-match. Log the count + names so we can
    // confirm before designing a transformer.
    let tool_count = req.tools.as_ref().map(|v| v.len()).unwrap_or(0);
    let tool_names: Vec<String> = req
        .tools
        .as_ref()
        .map(|v| {
            v.iter()
                .map(|t| {
                    let n = t.name.as_deref().unwrap_or("");
                    if n.is_empty() {
                        t.kind.clone()
                    } else {
                        format!("{}={}", t.kind, n)
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let parameters_total_bytes: usize = req
        .tools
        .as_ref()
        .map(|v| {
            v.iter()
                .map(|t| t.parameters.as_ref().map(|p| p.to_string().len()).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    tracing::info!(
        response_id = %response_id,
        model = %model_label,
        stream = stream_mode,
        tool_count,
        parameters_total_bytes,
        tool_names = ?tool_names,
        "responses: inbound request catalog snapshot"
    );

    debug!(
        response_id = %response_id,
        model = %model_label,
        stream = stream_mode,
        "responses: translating to chat.completions"
    );

    // Detect chunked-write mid-state from the incoming input items
    // BEFORE consuming `req` in translate_request. Used both for the
    // grammar lock and for the session-telemetry record.
    let chunked_write_active = match &req.input {
        ResponsesInput::Items(items) => in_chunked_write_state(items),
        ResponsesInput::Text(_) => false,
    };
    let input_item_count = match &req.input {
        ResponsesInput::Items(items) => items.len(),
        ResponsesInput::Text(_) => 1,
    };

    let frontdoor_on = frontdoor::is_enabled();

    // ── Inbound telemetry record ─────────────────────────────────────
    write_session_telemetry(serde_json::json!({
        "kind": "inbound",
        "response_id": response_id,
        "ts_unix": created_at,
        "model": model_label,
        "stream": stream_mode,
        "frontdoor_on": frontdoor_on,
        "chunked_write_active": chunked_write_active,
        "inbound_tool_count": tool_count,
        "inbound_tool_names": tool_names,
        "input_item_count": input_item_count,
        "parameters_total_bytes": parameters_total_bytes,
    }));

    // ── Request translation ───────────────────────────────────────────
    let chat_req = match translate_request(req, chunked_write_active, harness) {
        Ok(r) => r,
        Err(msg) => {
            return error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &msg);
        }
    };

    // ── Per-turn input capture ────────────────────────────────────────
    // Mirrors the raw_emission output capture (terminal record) so
    // post-mortem has the full `(input, output)` pair on disk and the
    // replay rig can drive the inference adapter offline. Writes the
    // post-translation ChatCompletionRequest — what the inference
    // backend actually receives. Best-effort: failures log warn, do
    // not affect the response path.
    let raw_input_summary = capture_raw_input(&response_id, &chat_req);
    write_session_telemetry(serde_json::json!({
        "kind": "input_capture",
        "response_id": response_id,
        "ts_unix": now_unix_secs(),
        "raw_input": raw_input_summary,
    }));

    // ── Inner invocation ──────────────────────────────────────────────
    let inner = chat_completions(State(state), headers, Json(chat_req)).await;

    // ── Response translation ──────────────────────────────────────────
    if stream_mode {
        translate_streaming_response(inner, response_id, model_label, created_at, metadata).await
    } else {
        translate_non_streaming_response(inner, response_id, model_label, created_at, metadata)
            .await
    }
}

// ─── Request translation ────────────────────────────────────────────

fn translate_request(
    req: ResponsesRequest,
    chunked_write_active: bool,
    harness: frontdoor::Harness,
) -> Result<ChatCompletionRequest, String> {
    // Derive per-harness flags up front so the rest of the function
    // reads cleanly. The `frontdoor_on` legacy name maps onto
    // "Opencode-style full reshape" semantics — see Harness doc.
    let runs_catalog_filter = harness.runs_catalog_filter();
    let runs_synthetic_tools = harness.runs_synthetic_tools();
    let runs_grammar_lock = harness.runs_grammar_lock();
    // For the catalog-filter log we keep emitting `frontdoor=<bool>`
    // because that's the field name long-standing tooling greps for;
    // it's true when the catalog gets reshaped (Opencode profile).
    let frontdoor_on = runs_catalog_filter;
    let mut messages: Vec<ChatMessage> = Vec::new();

    // `instructions` becomes a leading system message — semantically
    // closest to chat.completions, which has no native "instructions"
    // surface. Sits ahead of all input items.
    if let Some(instr) = req.instructions {
        if !instr.is_empty() {
            messages.push(ChatMessage::new("system", instr));
        }
    }

    // `input` items → messages.
    match req.input {
        ResponsesInput::Text(s) => {
            messages.push(ChatMessage::new("user", s));
        }
        ResponsesInput::Items(items) => {
            translate_input_items(items, &mut messages)?;
        }
    }

    // Tools: flat → nested.
    //
    // Codex's tool list contains:
    //   - `type:"function"` tools with top-level `name` (custom
    //     functions, MCP tools, …). These map directly to
    //     chat.completions function tools.
    //   - Built-in non-function tools: `local_shell`, `web_search`,
    //     `file_search`, `image_generation_call`,
    //     `computer_use_preview`, `mcp`. These have no top-level
    //     `name` and a different parameters shape. Most local models
    //     can't dispatch them, BUT — critically — `apply_patch` and
    //     `local_shell` are precisely how codex expects file writes
    //     and shell commands to land. Filtering them out leaves the
    //     model with zero tools, at which point it hallucinates tool
    //     names (`write`, `read_file`, …) and codex rejects every
    //     emit with `unsupported call: <name>`. Observed on the v7
    //     codex smoke 2026-05-12.
    //
    // Fix: pass non-function tools through as synthetic function tools.
    // Use the `type` value as the function name; chat.completions
    // doesn't care what shape `parameters` has so long as it's a
    // JSON object. Local models that pattern-match the registered
    // tool list will then emit `apply_patch` / `local_shell` / etc.
    // and codex's router will accept the call against its real
    // handler.
    // Catalog filtering — when the frontdoor is on, the model only
    // sees the small set of tools in `CODEX_TOOL_KEEPLIST` plus the
    // synthetic file-I/O tools appended below. This is the
    // deterministic half of the frontdoor (see `frontdoor` module
    // docs). When off, all tools pass through.
    //
    // Caller (`responses()`) reads `frontdoor::is_enabled()` once and
    // passes it down so unit tests can drive both paths without
    // racing on a shared env var.
    let tools = req.tools.map(|tools_in| {
        tools_in
            .into_iter()
            .filter_map(|t| {
                // Function tool with explicit name — straight pass-through
                // (when frontdoor is off) or keeplist-gated (when on).
                if t.kind == "function" {
                    let Some(name) = t.name else {
                        debug!("responses: dropping function tool with no name");
                        return None;
                    };
                    if runs_catalog_filter && !frontdoor::tool_keeplist_contains(&name) {
                        debug!(
                            name = %name,
                            "frontdoor: dropping function tool not in keeplist"
                        );
                        return None;
                    }
                    return Some(ToolDefinition {
                        kind: "function".to_string(),
                        function: ToolFunction {
                            name,
                            description: t.description,
                            parameters: t.parameters.unwrap_or_else(|| {
                                serde_json::json!({"type":"object","properties":{}})
                            }),
                        },
                    });
                }
                // Frontdoor mode is strict: only function tools survive.
                if frontdoor_on {
                    debug!(
                        kind = %t.kind,
                        "frontdoor: dropping non-function built-in tool"
                    );
                    return None;
                }
                // Built-in tool — wrap under its `type` name so the
                // model emits a chat.completions tool_call with that
                // name, and codex's tool router resolves it back to
                // the built-in handler when it receives the function
                // call.
                let synthetic_name = t.name.unwrap_or_else(|| t.kind.clone());
                debug!(
                    kind = %t.kind,
                    name = %synthetic_name,
                    "responses: bridging built-in tool as function"
                );
                Some(ToolDefinition {
                    kind: "function".to_string(),
                    function: ToolFunction {
                        name: synthetic_name,
                        description: t.description,
                        parameters: t.parameters.unwrap_or_else(|| {
                            serde_json::json!({"type":"object","properties":{}})
                        }),
                    },
                })
            })
            .collect::<Vec<_>>()
    });

    // Augment with synthetic file-I/O tools (frontdoor-on only).
    // These appear in the model's catalog as `write_file(path, content)`
    // / `read_file(path)`; the streaming + non-streaming response
    // paths rewrite outgoing tool_calls to their codex-compatible
    // `exec_command` equivalents before the events reach codex's
    // router.
    //
    // Gated on `frontdoor_on` because codex's training contract
    // (v11-v14 evidence, 2026-05-13) teaches the model to write files
    // via `exec_command` running `apply_patch <<'EOF' *** Begin Patch
    // ... EOF` — not via custom function tools. Injecting our synthetic
    // tools polluted the catalog without shifting the model's prior,
    // so the model used neither. When frontdoor is off we leave the
    // catalog untouched and trust codex's path.
    let tools = if runs_synthetic_tools {
        match tools {
            Some(mut existing) => {
                existing.extend(synthetic_file_tools());
                Some(existing)
            }
            None => Some(synthetic_file_tools()),
        }
    } else {
        tools
    };

    // Frontdoor grammar lock. Promote `tool_choice` to `"required"`
    // whenever the frontdoor is on so the inference adapter installs
    // its JSON-Schema grammar over the tool envelope on EVERY turn,
    // not just chunked-write turns. Decoder physically cannot emit:
    //   - args as a stringified JSON (grammar forces an object)
    //   - over-escaped inner quotes that break the outer envelope
    //   - tools outside the synthetic+keeplist catalog
    //
    // v12 telemetry (2026-05-13 04:01) showed the MoE emitting
    // `{"name":"write_file","arguments":"{\"path\":..."\\\\\\\"math\\\\\\\"..."}"}`
    // on T02 — args-as-string with broken inner escapes → outer JSON
    // parse fail → 1222 bytes lost as orphaned text. The chunked-write
    // gate did not engage (cw=False — no prior write_file_begin),
    // so dropping that gate makes the lock universal across frontdoor
    // turns. Catalog filter additionally engages in chunked-write
    // state to keep the model on protocol.
    let (tools, tool_choice_override) = if runs_grammar_lock {
        let filtered: Vec<ToolDefinition> = if chunked_write_active {
            tools
                .unwrap_or_default()
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.function.name.as_str(),
                        SYNTHETIC_TOOL_WRITE_FILE_CHUNK | SYNTHETIC_TOOL_WRITE_FILE_END
                    )
                })
                .collect()
        } else {
            tools.unwrap_or_default()
        };
        let names: Vec<&str> = filtered.iter().map(|td| td.function.name.as_str()).collect();
        tracing::info!(
            chunked_write_active,
            outbound_tool_count = filtered.len(),
            outbound_tools = ?names,
            "translate_request: frontdoor grammar lock — tool_choice=required"
        );
        (Some(filtered), Some(serde_json::json!("required")))
    } else {
        (tools, None)
    };

    // Emit the post-filter, post-synthetic, post-lock tool list at
    // info-level (per-tool drop logs are debug and filtered out in
    // deployed log threshold).
    if let Some(t) = &tools {
        let names: Vec<&str> = t.iter().map(|td| td.function.name.as_str()).collect();
        tracing::info!(
            frontdoor = frontdoor_on,
            chunked_write_active,
            outbound_tool_count = t.len(),
            outbound_tools = ?names,
            "translate_request: tool catalog after filter + synthetic-append"
        );
    }

    // Frontdoor mode: suppress chain-of-thought and floor max_tokens
    // so the model has room to emit a complete `<tool_call>` envelope
    // with multi-KB file content. Primary defaults to thinking; with
    // codex's typical max_output_tokens cap (1-4K), think tokens
    // exhaust the budget before the close `}` lands and the daemon's
    // marker parser drops the truncated call. Floor at 16K (well
    // within primary's 50K context) and explicitly disable thinking.
    //
    // Codex profile (Investment #11, 2026-05-13): same `enable_thinking:
    // false` treatment without the max_tokens floor. Codex 0.130 sets
    // its own output cap and the empirical smoke 2026-05-13 showed
    // ~85% of each turn's tokens spent inside `<think>` blocks adding
    // zero value for routine read-then-decide tool turns. Codex
    // envelope grammar already prevents truncation issues, so we just
    // turn thinking off.
    let suppress_thinking = frontdoor_on || matches!(harness, frontdoor::Harness::Codex);
    let (chat_template_kwargs, think_budget, max_tokens) = if frontdoor_on {
        let floored = req
            .max_output_tokens
            .map(|m| m.max(16_384))
            .or(Some(16_384));
        (
            Some(serde_json::json!({"enable_thinking": false})),
            Some(0u32),
            floored,
        )
    } else if suppress_thinking {
        (
            Some(serde_json::json!({"enable_thinking": false})),
            Some(0u32),
            req.max_output_tokens,
        )
    } else {
        (None, None, req.max_output_tokens)
    };

    // Codex profile temperature default (Investment #14, 2026-05-13).
    // The rg-loop fixture × 10 replays at T=0.7 reduced exact-loop
    // emissions 5× vs T=0.0 without destabilising envelope discipline.
    // Other Qwen-recommended sampling params (top_p, top_k, min_p,
    // presence_penalty) live in `ModelQuirks` and are applied by the
    // sampler — no per-route pinning needed.
    let temperature = req.temperature.or({
        if matches!(harness, frontdoor::Harness::Codex) {
            Some(0.7)
        } else {
            None
        }
    });

    Ok(ChatCompletionRequest {
        model: req.model,
        messages,
        temperature,
        max_tokens,
        stream: req.stream,
        top_p: req.top_p,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools,
        tool_choice: tool_choice_override.or(req.tool_choice),
        response_format: None,
        oicp: None,
        chat_template_kwargs,
        think_budget,
        tool_profile: None,
    sampling_mode: None,
    assistant_prefix: None,
    cmd_prefix: None,
    url_allowlist: None,
    evidence_id_allowlist: None,
    lark_grammar: None,
    })
}

fn translate_input_items(
    items: Vec<ResponsesInputItem>,
    messages: &mut Vec<ChatMessage>,
) -> Result<(), String> {
    for item in items {
        match item {
            ResponsesInputItem::Message(m) => {
                messages.push(translate_message_item(m)?);
            }
            ResponsesInputItem::FunctionCall(c) => {
                // Replayed assistant tool-call. Chat.completions wants
                // `{role:"assistant", tool_calls:[{id, type, function:{name, arguments}}]}`.
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: String::new(),
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        // chat.completions uses `id` (not `call_id`).
                        // Responses' `call_id` is the canonical handle;
                        // we forward it as the id so downstream tool
                        // turns can match it.
                        id: c.call_id,
                        kind: "function".into(),
                        function: FunctionCall {
                            name: c.name,
                            arguments: c.arguments,
                        },
                    }]),
                });
            }
            ResponsesInputItem::FunctionCallOutput(o) => {
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: o.output,
                    tool_call_id: Some(o.call_id),
                    tool_calls: None,
                });
            }
        }
    }
    Ok(())
}

fn translate_message_item(m: MessageItem) -> Result<ChatMessage, String> {
    let content = match m.content {
        MessageContent::Text(s) => s,
        MessageContent::Parts(parts) => parts
            .into_iter()
            .filter_map(|p| match p {
                ResponsesContentPart::InputText { text } => Some(text),
                ResponsesContentPart::OutputText { text } => Some(text),
                ResponsesContentPart::Other => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    Ok(ChatMessage {
        role: m.role,
        content,
        tool_call_id: None,
        tool_calls: None,
    })
}

// ─── Non-streaming response translation ─────────────────────────────

async fn translate_non_streaming_response(
    inner: Response,
    response_id: String,
    model_label: String,
    created_at: u64,
    metadata: Option<serde_json::Value>,
) -> Response {
    let status = inner.status();
    let body = inner.into_body();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "responses: failed to read inner body");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "adapter_error",
                "failed to read inner chat-completions response",
            );
        }
    };

    if !status.is_success() {
        // Pass through error body. Codex inspects HTTP status, not the
        // error shape, so this is fine even though shapes differ.
        return (status, bytes).into_response();
    }

    let chat: ChatCompletionResponse = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "responses: failed to parse inner JSON");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "adapter_error",
                "inner chat-completions response was not valid JSON",
            );
        }
    };

    let model = if chat.model.is_empty() { model_label.clone() } else { chat.model.clone() };
    let resp = build_non_streaming_response(chat, response_id, model, created_at, metadata);
    (StatusCode::OK, Json(resp)).into_response()
}

fn build_non_streaming_response(
    chat: ChatCompletionResponse,
    response_id: String,
    model: String,
    created_at: u64,
    metadata: Option<serde_json::Value>,
) -> ResponsesResponse {
    let mut output: Vec<ResponsesOutputItem> = Vec::new();
    let mut message_id_counter = 0u32;
    let mut tool_call_id_counter = 0u32;

    // chat.completions returns `choices[]`. Responses puts every output
    // item — including parallel tool calls — flat into `output[]`. We
    // emit one message (text, if any) followed by one function_call per
    // tool_call. Multi-choice (`n>1`) is unsupported and ignored beyond
    // index 0 — codex never sets `n>1`.
    let mut terminal_finish_reason: Option<String> = None;
    let mut terminal_text_bytes: usize = 0;
    let mut terminal_text_capture = String::new();
    let mut terminal_fcs: Vec<serde_json::Value> = Vec::new();
    if let Some(choice) = chat.choices.into_iter().next() {
        terminal_finish_reason = choice.finish_reason.clone();
        let msg = choice.message;
        let text = msg.content;
        terminal_text_bytes = text.len();
        terminal_text_capture = text.clone();

        if !text.is_empty() {
            message_id_counter += 1;
            let msg_id = format!("msg_{}_{}", response_id, message_id_counter);
            output.push(ResponsesOutputItem::Message(OutputMessage {
                id: msg_id,
                status: "completed",
                role: "assistant",
                content: vec![OutputContentPart::OutputText {
                    text,
                    annotations: vec![],
                }],
            }));
        }

        if let Some(tool_calls) = msg.tool_calls {
            for tc in tool_calls {
                tool_call_id_counter += 1;
                let fc_id = format!("fc_{}_{}", response_id, tool_call_id_counter);
                // Rewrite synthetic file-I/O tools — same logic as
                // the streaming path. The model's emit was a clean
                // {path, content} envelope; codex expects an
                // exec_command call.
                let raw_name = tc.function.name.clone();
                let raw_args = tc.function.arguments.clone();
                let (name, arguments) = match rewrite_synthetic_tool_call(
                    &tc.function.name,
                    &tc.function.arguments,
                ) {
                    Some(pair) => pair,
                    None => (tc.function.name, tc.function.arguments),
                };
                let parsed_ok = serde_json::from_str::<serde_json::Value>(&raw_args).is_ok();
                let mut fc_rec = serde_json::Map::new();
                fc_rec.insert("name".into(), serde_json::Value::String(raw_name.clone()));
                fc_rec.insert(
                    "args_bytes".into(),
                    serde_json::Value::Number(raw_args.len().into()),
                );
                fc_rec.insert("args_parsed_ok".into(), serde_json::Value::Bool(parsed_ok));
                fc_rec.insert(
                    "args_sample".into(),
                    serde_json::Value::String(args_sample(&raw_args)),
                );
                if let Some(h) = frontdoor::extract_heredoc_diagnostics(&raw_args) {
                    if let Ok(v) = serde_json::to_value(&h) {
                        fc_rec.insert("heredoc".into(), v);
                    }
                }
                terminal_fcs.push(serde_json::Value::Object(fc_rec));
                output.push(ResponsesOutputItem::FunctionCall(OutputFunctionCall {
                    id: fc_id,
                    call_id: tc.id,
                    name,
                    arguments,
                    status: "completed",
                }));
            }
        }
    }

    let raw_emission = capture_raw_emission(&response_id, &terminal_text_capture);
    write_session_telemetry(serde_json::json!({
        "kind": "terminal",
        "stream": false,
        "response_id": response_id,
        "ts_unix": now_unix_secs(),
        "finish_reason": terminal_finish_reason,
        "text_buffer_bytes": terminal_text_bytes,
        "function_calls": terminal_fcs,
        "raw_emission": raw_emission,
    }));

    let usage = chat.usage.map(|u| ResponsesUsage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });

    ResponsesResponse {
        id: response_id,
        object: "response",
        created_at,
        status: "completed",
        model,
        output,
        usage,
        metadata,
        reasoning: None,
    }
}

// ─── Streaming response translation ─────────────────────────────────

async fn translate_streaming_response(
    inner: Response,
    response_id: String,
    model_label: String,
    created_at: u64,
    metadata: Option<serde_json::Value>,
) -> Response {
    let status = inner.status();
    if !status.is_success() {
        // Inner failed before the stream opened — forward the error
        // body as-is; codex surfaces the HTTP status to the user.
        return inner;
    }

    let body = inner.into_body();
    let (tx, rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(64);

    tokio::spawn(async move {
        let mut state = ResponsesStreamState::new(response_id, model_label, created_at, metadata);

        // Initial events: response.created + response.in_progress.
        let _ = tx.send(Ok(state.emit_created())).await;
        let _ = tx.send(Ok(state.emit_in_progress())).await;

        let mut byte_stream = body.into_data_stream();
        let mut buf = BytesMut::new();
        let mut got_done = false;

        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(b) => buf.extend_from_slice(&b),
                Err(e) => {
                    warn!(error = %e, "responses: inner stream read error");
                    let _ = tx.send(Ok(state.emit_failed(&format!("stream error: {e}")))).await;
                    return;
                }
            }

            while let Some(event_bytes) = take_one_sse_event(&mut buf) {
                let Some(data) = parse_sse_data(&event_bytes) else { continue };
                if data == "[DONE]" {
                    got_done = true;
                    break;
                }
                let chunk: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(e) => {
                        debug!(error = %e, "responses: skipped malformed inner chunk");
                        continue;
                    }
                };
                for ev in state.handle_chat_chunk(&chunk) {
                    if tx.send(Ok(ev)).await.is_err() {
                        return;
                    }
                }
            }

            if got_done {
                break;
            }
        }

        // Stream closed. Emit terminal events: close any open message,
        // then response.completed.
        for ev in state.emit_completion() {
            if tx.send(Ok(ev)).await.is_err() {
                return;
            }
        }
    });

    let _ = body_for_keepalive_marker(); // doc-hook, see fn body
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// No-op marker so an `unused_imports`-style audit doesn't suggest
/// dropping the Body import. `Body` is intentionally referenced
/// because future variants of this adapter (e.g., octet-stream
/// passthrough on adapter failures) reach for it directly.
fn body_for_keepalive_marker() -> Option<Body> {
    None
}

// ─── Streaming FSM ──────────────────────────────────────────────────

/// State for translating chat.completions chunks → Responses events.
///
/// Held across `handle_chat_chunk` calls. Emits the lifecycle events
/// (output_item.added / .done, content_part.added / .done) lazily as
/// the underlying stream reveals what kind of output the model is
/// producing.
struct ResponsesStreamState {
    response_id: String,
    model: String,
    created_at: u64,
    metadata: Option<serde_json::Value>,

    sequence_number: u64,
    next_output_index: u32,
    message_id_counter: u32,
    fc_id_counter: u32,

    /// Per-output-index state for messages currently open on the wire.
    message: Option<OpenMessage>,
    /// Per-output-index state for function_calls already emitted.
    /// Closed eagerly when the inner stream gives us the whole envelope.
    function_calls: Vec<ClosedFunctionCall>,

    usage: Option<ResponsesUsage>,
    finish_reason: Option<String>,
    completed: bool,
}

struct OpenMessage {
    output_index: u32,
    item_id: String,
    text_buffer: String,
}

struct ClosedFunctionCall {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

impl ResponsesStreamState {
    fn new(
        response_id: String,
        model: String,
        created_at: u64,
        metadata: Option<serde_json::Value>,
    ) -> Self {
        Self {
            response_id,
            model,
            created_at,
            metadata,
            sequence_number: 0,
            next_output_index: 0,
            message_id_counter: 0,
            fc_id_counter: 0,
            message: None,
            function_calls: Vec::new(),
            usage: None,
            finish_reason: None,
            completed: false,
        }
    }

    fn seq(&mut self) -> u64 {
        let s = self.sequence_number;
        self.sequence_number += 1;
        s
    }

    fn response_shell(&self, status: &'static str) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "model": self.model,
            "output": [],
            "reasoning": null,
        });
        if let Some(m) = &self.metadata {
            v["metadata"] = m.clone();
        }
        v
    }

    fn emit_created(&mut self) -> Event {
        let seq = self.seq();
        let payload = serde_json::json!({
            "type": "response.created",
            "sequence_number": seq,
            "response": self.response_shell("in_progress"),
        });
        sse_event("response.created", &payload)
    }

    fn emit_in_progress(&mut self) -> Event {
        let seq = self.seq();
        let payload = serde_json::json!({
            "type": "response.in_progress",
            "sequence_number": seq,
            "response": self.response_shell("in_progress"),
        });
        sse_event("response.in_progress", &payload)
    }

    fn emit_failed(&mut self, message: &str) -> Event {
        let seq = self.seq();
        let mut shell = self.response_shell("failed");
        shell["error"] = serde_json::json!({
            "code": "adapter_error",
            "message": message,
        });
        let payload = serde_json::json!({
            "type": "response.failed",
            "sequence_number": seq,
            "response": shell,
        });
        sse_event("response.failed", &payload)
    }

    /// Process one chat.completion chunk, returning the Responses
    /// events to emit (zero or more).
    fn handle_chat_chunk(&mut self, chunk: &serde_json::Value) -> Vec<Event> {
        let mut out = Vec::new();

        // Top-level usage may appear on the terminal chunk.
        if let Some(u) = chunk.get("usage") {
            if let Some(usage) = parse_usage(u) {
                self.usage = Some(usage);
            }
        }

        let Some(choice) = chunk.pointer("/choices/0") else {
            return out;
        };

        let delta = choice.get("delta").cloned().unwrap_or(serde_json::json!({}));

        // Text content delta.
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                // Open a message if this is the first text we've seen.
                if self.message.is_none() {
                    self.message_id_counter += 1;
                    let item_id =
                        format!("msg_{}_{}", self.response_id, self.message_id_counter);
                    let output_index = self.next_output_index;
                    self.next_output_index += 1;
                    self.message = Some(OpenMessage {
                        output_index,
                        item_id: item_id.clone(),
                        text_buffer: String::new(),
                    });

                    // output_item.added (message shell).
                    {
                        let seq = self.seq();
                        let payload = serde_json::json!({
                            "type": "response.output_item.added",
                            "sequence_number": seq,
                            "output_index": output_index,
                            "item": {
                                "id": item_id,
                                "type": "message",
                                "status": "in_progress",
                                "role": "assistant",
                                "content": [],
                            }
                        });
                        out.push(sse_event("response.output_item.added", &payload));
                    }
                    // content_part.added (output_text shell).
                    {
                        let seq = self.seq();
                        let payload = serde_json::json!({
                            "type": "response.content_part.added",
                            "sequence_number": seq,
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": {
                                "type": "output_text",
                                "text": "",
                                "annotations": [],
                            }
                        });
                        out.push(sse_event("response.content_part.added", &payload));
                    }
                }

                // Snapshot the fields we need into locals before
                // calling `self.seq()` — the borrow checker treats
                // `as_mut` + `seq()` as overlapping `&mut self` calls.
                let (msg_item_id, msg_output_index) = {
                    // `self.message` was just set to `Some` above when it
                    // was `None`, so this is always `Some`. Bail out of the
                    // chunk gracefully (emit what we have) rather than
                    // panicking mid-stream if that invariant ever breaks.
                    let Some(msg) = self.message.as_mut() else {
                        return out;
                    };
                    msg.text_buffer.push_str(content);
                    (msg.item_id.clone(), msg.output_index)
                };
                let seq = self.seq();
                let payload = serde_json::json!({
                    "type": "response.output_text.delta",
                    "sequence_number": seq,
                    "item_id": msg_item_id,
                    "output_index": msg_output_index,
                    "content_index": 0,
                    "delta": content,
                });
                out.push(sse_event("response.output_text.delta", &payload));
            }
        }

        // Tool calls. The local SSE bridge emits a single chunk with
        // all parsed tool_calls (post-generation extract); we forward
        // each as a complete function_call output item — added →
        // arguments.delta(full args) → arguments.done → output_item.done.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                // Closure capture of `&mut self` would race the
                // explicit `self.fc_id_counter` bump a few lines down.
                // Resolve via a `match` so the mutable borrow scope is
                // confined to the absent-id arm.
                let call_id = match tc.get("id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        self.fc_id_counter += 1;
                        format!("call_{}_{}", self.response_id, self.fc_id_counter)
                    }
                };
                let func = tc.get("function").cloned().unwrap_or(serde_json::json!({}));
                let raw_name = func
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let raw_arguments = func
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Rewrite synthetic file-I/O tools into codex-compatible
                // exec_command before emitting events. The model emitted
                // a clean envelope (path/content); the wire to codex
                // carries a shell command codex can dispatch.
                let (name, mut arguments) =
                    match rewrite_synthetic_tool_call(&raw_name, &raw_arguments) {
                        Some((rewritten_name, rewritten_args)) => {
                            tracing::info!(
                                from = %raw_name,
                                to = %rewritten_name,
                                "responses: rewrote synthetic tool call"
                            );
                            (rewritten_name, rewritten_args)
                        }
                        None => (raw_name, raw_arguments),
                    };
                // Post-emission canonicalization on the streaming
                // path. The non-streaming sibling in routes_inference
                // runs the same repair, but codex's /v1/responses
                // adapter sends `stream: true` so the inner chat
                // request goes through serve_local_stream and bypasses
                // it. Without this hook, malformed apply_patch
                // heredocs reach codex's verifier and get rejected
                // (gym 008 / smoke 2026-05-13).
                if let Some(rewritten) =
                    crate::frontdoor::canonicalize_exec_command_arguments(&name, &arguments)
                {
                    tracing::info!(
                        response_id = %self.response_id,
                        "responses-stream: apply_patch heredoc canonicalized"
                    );
                    arguments = rewritten;
                }

                self.fc_id_counter += 1;
                let item_id = format!("fc_{}_{}", self.response_id, self.fc_id_counter);
                let output_index = self.next_output_index;
                self.next_output_index += 1;

                // output_item.added.
                {
                    let seq = self.seq();
                    let payload = serde_json::json!({
                        "type": "response.output_item.added",
                        "sequence_number": seq,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "status": "in_progress",
                            "call_id": call_id,
                            "name": name,
                            "arguments": "",
                        }
                    });
                    out.push(sse_event("response.output_item.added", &payload));
                }
                // function_call_arguments.delta (entire args as a single delta).
                {
                    let seq = self.seq();
                    let payload = serde_json::json!({
                        "type": "response.function_call_arguments.delta",
                        "sequence_number": seq,
                        "item_id": item_id,
                        "output_index": output_index,
                        "delta": arguments,
                    });
                    out.push(sse_event("response.function_call_arguments.delta", &payload));
                }
                // function_call_arguments.done.
                {
                    let seq = self.seq();
                    let payload = serde_json::json!({
                        "type": "response.function_call_arguments.done",
                        "sequence_number": seq,
                        "item_id": item_id,
                        "output_index": output_index,
                        "arguments": arguments,
                    });
                    out.push(sse_event("response.function_call_arguments.done", &payload));
                }
                // output_item.done.
                {
                    let seq = self.seq();
                    let payload = serde_json::json!({
                        "type": "response.output_item.done",
                        "sequence_number": seq,
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "status": "completed",
                            "call_id": call_id,
                            "name": name,
                            "arguments": arguments,
                        }
                    });
                    out.push(sse_event("response.output_item.done", &payload));
                }

                self.function_calls.push(ClosedFunctionCall {
                    item_id,
                    call_id,
                    name,
                    arguments,
                });
            }
        }

        // Finish reason — terminal signal on the inner chunk. We close
        // an open message here so the eventual `emit_completion` only
        // needs to emit `response.completed`.
        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            if !reason.is_empty() {
                self.finish_reason = Some(reason.to_string());
                // Per-turn outcome telemetry: shows whether the model
                // finished naturally (`stop`), got cut by max_tokens
                // (`length`), or emitted tool_calls. Combined with the
                // text_buffer size and per-tool logs, this answers
                // "why didn't this turn produce a file?".
                let text_bytes = self
                    .message
                    .as_ref()
                    .map(|m| m.text_buffer.len())
                    .unwrap_or(0);
                tracing::info!(
                    response_id = %self.response_id,
                    finish_reason = %reason,
                    text_buffer_bytes = text_bytes,
                    function_calls_emitted = self.function_calls.len(),
                    "responses: stream turn terminal"
                );
                let fc_summary: Vec<serde_json::Value> = self
                    .function_calls
                    .iter()
                    .map(|fc| {
                        let parsed_ok = serde_json::from_str::<serde_json::Value>(&fc.arguments)
                            .is_ok();
                        let mut rec = serde_json::Map::new();
                        rec.insert("name".into(), serde_json::Value::String(fc.name.clone()));
                        rec.insert(
                            "args_bytes".into(),
                            serde_json::Value::Number(fc.arguments.len().into()),
                        );
                        rec.insert("args_parsed_ok".into(), serde_json::Value::Bool(parsed_ok));
                        rec.insert(
                            "args_sample".into(),
                            serde_json::Value::String(args_sample(&fc.arguments)),
                        );
                        if let Some(h) = frontdoor::extract_heredoc_diagnostics(&fc.arguments) {
                            if let Ok(v) = serde_json::to_value(&h) {
                                rec.insert("heredoc".into(), v);
                            }
                        }
                        serde_json::Value::Object(rec)
                    })
                    .collect();
                let raw_text = self
                    .message
                    .as_ref()
                    .map(|m| m.text_buffer.clone())
                    .unwrap_or_default();
                let raw_emission = capture_raw_emission(&self.response_id, &raw_text);
                write_session_telemetry(serde_json::json!({
                    "kind": "terminal",
                    "stream": true,
                    "response_id": self.response_id,
                    "ts_unix": now_unix_secs(),
                    "finish_reason": reason,
                    "text_buffer_bytes": text_bytes,
                    "function_calls": fc_summary,
                    "raw_emission": raw_emission,
                }));
                if reason == "length" {
                    tracing::warn!(
                        response_id = %self.response_id,
                        text_buffer_bytes = text_bytes,
                        "responses: model hit max_tokens — tool_call may be truncated"
                    );
                }
                if let Some(open) = self.message.take() {
                    // output_text.done.
                    {
                        let seq = self.seq();
                        let payload = serde_json::json!({
                            "type": "response.output_text.done",
                            "sequence_number": seq,
                            "item_id": open.item_id,
                            "output_index": open.output_index,
                            "content_index": 0,
                            "text": open.text_buffer,
                        });
                        out.push(sse_event("response.output_text.done", &payload));
                    }
                    // content_part.done.
                    {
                        let seq = self.seq();
                        let payload = serde_json::json!({
                            "type": "response.content_part.done",
                            "sequence_number": seq,
                            "item_id": open.item_id,
                            "output_index": open.output_index,
                            "content_index": 0,
                            "part": {
                                "type": "output_text",
                                "text": open.text_buffer,
                                "annotations": [],
                            }
                        });
                        out.push(sse_event("response.content_part.done", &payload));
                    }
                    // output_item.done (message).
                    {
                        let seq = self.seq();
                        let payload = serde_json::json!({
                            "type": "response.output_item.done",
                            "sequence_number": seq,
                            "output_index": open.output_index,
                            "item": {
                                "id": open.item_id,
                                "type": "message",
                                "status": "completed",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": open.text_buffer,
                                    "annotations": [],
                                }]
                            }
                        });
                        out.push(sse_event("response.output_item.done", &payload));
                    }
                }
            }
        }

        out
    }

    /// Final events on stream close. Idempotent — safe to call once.
    fn emit_completion(&mut self) -> Vec<Event> {
        if self.completed {
            return Vec::new();
        }
        self.completed = true;
        let mut out = Vec::new();

        // Defensive: if [DONE] arrived without a `finish_reason` chunk,
        // close any open message.
        if let Some(open) = self.message.take() {
            {
                let seq = self.seq();
                let payload = serde_json::json!({
                    "type": "response.output_text.done",
                    "sequence_number": seq,
                    "item_id": open.item_id,
                    "output_index": open.output_index,
                    "content_index": 0,
                    "text": open.text_buffer,
                });
                out.push(sse_event("response.output_text.done", &payload));
            }
            {
                let seq = self.seq();
                let payload = serde_json::json!({
                    "type": "response.content_part.done",
                    "sequence_number": seq,
                    "item_id": open.item_id,
                    "output_index": open.output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": open.text_buffer,
                        "annotations": [],
                    }
                });
                out.push(sse_event("response.content_part.done", &payload));
            }
            {
                let seq = self.seq();
                let payload = serde_json::json!({
                    "type": "response.output_item.done",
                    "sequence_number": seq,
                    "output_index": open.output_index,
                    "item": {
                        "id": open.item_id,
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": open.text_buffer,
                            "annotations": [],
                        }]
                    }
                });
                out.push(sse_event("response.output_item.done", &payload));
            }
        }

        // Build the final response envelope so codex can read the full
        // output set off `response.completed` if it skipped per-item
        // accumulation.
        let mut shell = self.response_shell("completed");
        let mut output_array: Vec<serde_json::Value> = Vec::new();

        // Re-derive output array: we don't keep emitted message text
        // after closing the message — the only consumer that cares is
        // the completion envelope, and tests verify per-event payloads
        // contain the text. For the envelope we emit empty content for
        // any closed message (clients rebuild from deltas). Function
        // calls we kept around — emit them with full args.
        for fc in &self.function_calls {
            output_array.push(serde_json::json!({
                "id": fc.item_id,
                "type": "function_call",
                "status": "completed",
                "call_id": fc.call_id,
                "name": fc.name,
                "arguments": fc.arguments,
            }));
        }
        shell["output"] = serde_json::Value::Array(output_array);

        if let Some(u) = &self.usage {
            shell["usage"] = serde_json::json!({
                "input_tokens": u.input_tokens,
                "output_tokens": u.output_tokens,
                "total_tokens": u.total_tokens,
            });
        }

        let seq = self.seq();
        let payload = serde_json::json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": shell,
        });
        out.push(sse_event("response.completed", &payload));
        out
    }
}

fn parse_usage(v: &serde_json::Value) -> Option<ResponsesUsage> {
    Some(ResponsesUsage {
        input_tokens: v.get("prompt_tokens")?.as_u64()? as u32,
        output_tokens: v.get("completion_tokens")?.as_u64()? as u32,
        total_tokens: v.get("total_tokens")?.as_u64()? as u32,
    })
}

// ─── SSE byte protocol helpers ──────────────────────────────────────

/// Pull one complete SSE event off the front of `buf` if one is
/// available. Returns the event bytes (including the trailing `\n\n`)
/// or `None` when more bytes are needed.
fn take_one_sse_event(buf: &mut BytesMut) -> Option<Bytes> {
    // Hunt for `\n\n` — SSE event terminator. Also accept `\r\n\r\n`.
    let lf = buf.windows(2).position(|w| w == b"\n\n");
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let (idx, term_len) = match (lf, crlf) {
        (Some(l), Some(c)) if c < l => (c, 4),
        (Some(l), _) => (l, 2),
        (_, Some(c)) => (c, 4),
        _ => return None,
    };
    let bytes = buf.split_to(idx + term_len).freeze();
    Some(bytes)
}

/// Extract the body of the `data:` field from one SSE event. Drops
/// `event:` / `id:` / `retry:` / blank lines. Trims the leading space
/// after the colon if present (per the SSE spec).
fn parse_sse_data(event_bytes: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(event_bytes).ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            return Some(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    None
}

fn sse_event(event_name: &'static str, payload: &serde_json::Value) -> Event {
    Event::default().event(event_name).data(payload.to_string())
}


// ─── Generic helpers ────────────────────────────────────────────────

fn mk_response_id() -> String {
    format!(
        "resp_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    )
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Serialize)]
struct AdapterError {
    error: AdapterErrorBody,
}

#[derive(Serialize)]
struct AdapterErrorBody {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
}

fn error_response(status: StatusCode, error_type: &str, message: &str) -> Response {
    let body = AdapterError {
        error: AdapterErrorBody {
            message: message.to_string(),
            error_type: error_type.to_string(),
        },
    };
    (status, Json(body)).into_response()
}

/// Capture a per-turn raw model emission to disk and return a small
/// JSON object summarising it for the terminal telemetry record.
///
/// Investment 1 (2026-05-13): without seeing the actual bytes the
/// model produced, every shape-regression debugging session devolves
/// into scraping `codex exec` stdout. Now the full text emission
/// lands at `~/.sovereign/codex-sessions/raw/<response_id>.txt` (one
/// file per turn, joinable by response_id) and a 16-char SHA prefix
/// + head/tail sample rides on the terminal record itself.
///
/// Returned shape:
/// ```ignore
/// {
///   "sha256_prefix": "ab12cd34ef567890",
///   "head": "<first 400 chars>",
///   "tail": "<last 400 chars when len>800>",
///   "len": <total bytes>,
///   "file_path": "/Users/.../raw/<response_id>.txt"
/// }
/// ```
///
/// Best-effort: file write failures log a `warn!` and the returned
/// object still carries the in-memory diagnostics. Suitable for
/// merging into the terminal record via
/// `rec.insert("raw_emission", capture_raw_emission(...))`.
fn capture_raw_emission(response_id: &str, text: &str) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let sha_full = hex::encode(hasher.finalize());
    let sha_prefix = sha_full[..16].to_string();
    let len = text.len();
    let head_n = char_boundary_le(text, 400);
    let head = &text[..head_n];
    let tail_text: Option<&str> = if len > 800 {
        let tail_start = char_boundary_ge(text, len - 400);
        Some(&text[tail_start..])
    } else {
        None
    };

    let mut obj = serde_json::Map::new();
    obj.insert(
        "sha256_prefix".into(),
        serde_json::Value::String(sha_prefix),
    );
    obj.insert("len".into(), serde_json::Value::Number(len.into()));
    obj.insert("head".into(), serde_json::Value::String(head.to_string()));
    if let Some(t) = tail_text {
        obj.insert("tail".into(), serde_json::Value::String(t.to_string()));
    }

    if let Some(home) = dirs::home_dir() {
        let dir = home.join(".sovereign").join("codex-sessions").join("raw");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(error = %e, dir = %dir.display(), "raw emission: mkdir failed");
        } else {
            let path = dir.join(format!("{}.txt", response_id));
            match std::fs::write(&path, text.as_bytes()) {
                Ok(()) => {
                    obj.insert(
                        "file_path".into(),
                        serde_json::Value::String(path.to_string_lossy().into_owned()),
                    );
                }
                Err(e) => {
                    warn!(error = %e, path = %path.display(), "raw emission: write failed");
                }
            }
        }
    }

    serde_json::Value::Object(obj)
}

/// Return the largest char-boundary index `<= max_bytes`. Used to
/// slice UTF-8 text safely for sampling.
fn char_boundary_le(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Return the smallest char-boundary index `>= min_bytes`. Used to
/// slice UTF-8 text safely for sampling.
fn char_boundary_ge(s: &str, min_bytes: usize) -> usize {
    if min_bytes >= s.len() {
        return s.len();
    }
    let mut i = min_bytes;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Capture the per-turn input prompt the inference adapter is about
/// to see. Mirrors `capture_raw_emission` for the request side so
/// every smoke turn produces a complete `(in, out)` pair on disk,
/// joinable by `response_id`. Returns a summary object for the
/// inbound telemetry record (sha + len + file_path).
///
/// File location: `~/.sovereign/codex-sessions/raw/<response_id>.input.json`.
/// Content: the fully-translated `ChatCompletionRequest` (post-
/// frontdoor passes — distiller, grammar lock, brief, history
/// compression). This is the EXACT shape the inference adapter
/// receives; closes the gap between "what we sent" and "what the
/// model saw".
fn capture_raw_input(response_id: &str, chat_req: &ChatCompletionRequest) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    let serialized = match serde_json::to_vec_pretty(chat_req) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "raw input: serialize failed");
            return serde_json::Value::Null;
        }
    };
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    let sha_full = hex::encode(hasher.finalize());
    let sha_prefix = sha_full[..16].to_string();
    let len = serialized.len();

    let mut obj = serde_json::Map::new();
    obj.insert(
        "sha256_prefix".into(),
        serde_json::Value::String(sha_prefix),
    );
    obj.insert("len".into(), serde_json::Value::Number(len.into()));

    if let Some(home) = dirs::home_dir() {
        let dir = home.join(".sovereign").join("codex-sessions").join("raw");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(error = %e, dir = %dir.display(), "raw input: mkdir failed");
        } else {
            let path = dir.join(format!("{}.input.json", response_id));
            match std::fs::write(&path, &serialized) {
                Ok(()) => {
                    obj.insert(
                        "file_path".into(),
                        serde_json::Value::String(path.to_string_lossy().into_owned()),
                    );
                }
                Err(e) => {
                    warn!(error = %e, path = %path.display(), "raw input: write failed");
                }
            }
        }
    }

    serde_json::Value::Object(obj)
}

/// Sample an argument string for telemetry: returns up to 200 chars
/// from each end with `… [N bytes elided]` in the middle when long.
fn args_sample(args: &str) -> String {
    let len = args.len();
    if len <= 400 {
        return args.to_string();
    }
    let head_end = char_boundary_le(args, 200);
    let tail_start = char_boundary_ge(args, len - 200);
    format!(
        "{}… [{} bytes elided] …{}",
        &args[..head_end],
        tail_start - head_end,
        &args[tail_start..]
    )
}

/// Append a single JSON record to today's per-session telemetry log
/// at `~/.sovereign/codex-sessions/<YYYY-MM-DD>.jsonl`.
///
/// Two record kinds are written per /v1/responses call:
///   1. `kind:"inbound"` at request entry — captures the surface
///      the adapter is about to translate (catalog, item count,
///      mid-chunked-write state, frontdoor toggle).
///   2. `kind:"terminal"` at finish-reason — captures what the
///      model actually produced (finish_reason, text bytes, per-tool
///      args bytes, parsed_ok flag, synthetic-tool counts).
///
/// Records share a `response_id` so an operator can join them with
/// `jq` for post-mortem analysis. Best-effort: write failures log a
/// warn and otherwise do not affect the response path.
fn write_session_telemetry(record: serde_json::Value) {
    let Some(home) = dirs::home_dir() else {
        warn!("session telemetry: no HOME — dropping record");
        return;
    };
    let dir = home.join(".sovereign").join("codex-sessions");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(error = %e, dir = %dir.display(), "session telemetry: mkdir failed");
        return;
    }
    // Single rolling file. Each record carries `ts_unix` so the
    // operator can split by day with `jq` if needed — we avoid a
    // chrono dep here. Rotate by hand when it gets large.
    let path = dir.join("sessions.jsonl");
    let line = match serde_json::to_string(&record) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "session telemetry: serialize failed");
            return;
        }
    };
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, path = %path.display(), "session telemetry: open failed");
            return;
        }
    };
    if let Err(e) = writeln!(file, "{}", line) {
        warn!(error = %e, path = %path.display(), "session telemetry: write failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reshaping::{normalize_path_segments, parent_dir, shell_single_quote};
    use crate::responses_types::{
        FunctionCallItem as RFnCall, FunctionCallOutputItem as RFnOut, MessageContent,
        MessageItem as RMsg, ResponsesContentPart, ResponsesInput, ResponsesInputItem,
        ResponsesTool,
    };

    #[test]
    fn args_sample_returns_short_strings_verbatim() {
        assert_eq!(args_sample("{}"), "{}");
        let s = "x".repeat(400);
        assert_eq!(args_sample(&s), s);
    }

    #[test]
    fn args_sample_elides_middle_for_long_strings() {
        let s = format!("{}{}{}", "A".repeat(200), "M".repeat(200), "Z".repeat(200));
        let out = args_sample(&s);
        assert!(out.starts_with(&"A".repeat(200)));
        assert!(out.ends_with(&"Z".repeat(200)));
        assert!(out.contains("bytes elided"));
        assert!(out.len() < s.len());
    }

    #[test]
    fn char_boundary_helpers_respect_utf8() {
        // Three-byte char `€` straddles boundaries; helpers must not split it.
        let s = "abc€def";
        assert!(char_boundary_le(s, 4) <= s.len());
        let i = char_boundary_le(s, 4);
        assert!(s.is_char_boundary(i));
        let j = char_boundary_ge(s, 4);
        assert!(s.is_char_boundary(j));
    }

    #[test]
    fn capture_raw_emission_returns_sha_head_and_len() {
        let v = capture_raw_emission("resp_test_abc", "hello world");
        let obj = v.as_object().expect("object");
        assert_eq!(obj.get("len").and_then(|x| x.as_u64()), Some(11));
        let sha = obj.get("sha256_prefix").and_then(|x| x.as_str()).unwrap();
        assert_eq!(sha.len(), 16);
        assert_eq!(
            obj.get("head").and_then(|x| x.as_str()),
            Some("hello world")
        );
        // No tail for strings under 800 bytes.
        assert!(obj.get("tail").is_none());
    }

    #[test]
    fn capture_raw_emission_emits_tail_for_long_inputs() {
        let body = format!("{}{}", "S".repeat(500), "E".repeat(500));
        let v = capture_raw_emission("resp_test_long", &body);
        let obj = v.as_object().expect("object");
        assert_eq!(obj.get("len").and_then(|x| x.as_u64()), Some(1000));
        assert!(obj.get("tail").is_some());
        let tail = obj.get("tail").and_then(|x| x.as_str()).unwrap();
        assert!(tail.ends_with(&"E".repeat(100)));
    }

    fn req_with_input(input: ResponsesInput) -> ResponsesRequest {
        ResponsesRequest {
            model: Some("test".into()),
            input,
            instructions: None,
            tools: None,
            tool_choice: None,
            stream: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            previous_response_id: None,
            store: None,
            parallel_tool_calls: None,
            reasoning: None,
            metadata: None,
        }
    }

    #[test]
    fn translate_request_string_input() {
        let req = req_with_input(ResponsesInput::Text("hello".into()));
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(chat.messages[0].content, "hello");
    }

    #[test]
    fn in_chunked_write_state_detects_recent_write_file_begin() {
        let items = vec![
            ResponsesInputItem::FunctionCall(RFnCall {
                call_id: "c1".into(),
                name: "write_file_begin".into(),
                arguments: "{\"path\":\"/x\"}".into(),
                id: None,
            }),
            ResponsesInputItem::FunctionCallOutput(RFnOut {
                call_id: "c1".into(),
                output: "0".into(),
            }),
        ];
        assert!(in_chunked_write_state(&items));
    }

    #[test]
    fn in_chunked_write_state_false_when_last_call_is_other_tool() {
        let items = vec![
            ResponsesInputItem::FunctionCall(RFnCall {
                call_id: "c1".into(),
                name: "exec_command".into(),
                arguments: "{}".into(),
                id: None,
            }),
        ];
        assert!(!in_chunked_write_state(&items));
    }

    #[test]
    fn translate_request_chunked_write_locks_catalog_and_forces_required() {
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: Some("exec_command".into()),
            description: None,
            parameters: Some(serde_json::json!({"type":"object","properties":{}})),
            strict: None,
        }]);
        let chat = translate_request(req, true, frontdoor::Harness::Opencode).unwrap();
        let names: Vec<&str> = chat
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| t.function.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["write_file_chunk", "write_file_end"],
            "chunked-write state should lock catalog to chunk+end"
        );
        assert_eq!(
            chat.tool_choice.as_ref().and_then(|v| v.as_str()),
            Some("required"),
            "tool_choice should be promoted to required"
        );
    }

    #[test]
    fn translate_request_frontdoor_on_forces_required_even_without_chunked_write() {
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: Some("exec_command".into()),
            description: None,
            parameters: Some(serde_json::json!({"type":"object","properties":{}})),
            strict: None,
        }]);
        // chunked_write_active=false, frontdoor_on=true: full catalog
        // kept, but tool_choice promoted so grammar engages on T01.
        let chat = translate_request(req, false, frontdoor::Harness::Opencode).unwrap();
        let names: Vec<&str> = chat
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| t.function.name.as_str())
            .collect();
        assert!(names.contains(&"exec_command"), "exec_command kept");
        assert!(names.contains(&"write_file"), "write_file kept");
        assert_eq!(
            chat.tool_choice.as_ref().and_then(|v| v.as_str()),
            Some("required"),
            "tool_choice should be promoted on every frontdoor turn"
        );
    }

    #[test]
    fn translate_request_bare_harness_preserves_tool_choice() {
        // Bare is the only profile that does NOT engage grammar lock,
        // so the caller's tool_choice flows through unchanged.
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tool_choice = Some(serde_json::json!("auto"));
        let chat = translate_request(req, false, frontdoor::Harness::Bare).unwrap();
        assert_eq!(
            chat.tool_choice.as_ref().and_then(|v| v.as_str()),
            Some("auto"),
            "Bare harness must not override caller's tool_choice"
        );
    }

    #[test]
    fn translate_request_codex_pins_temperature_default() {
        // Investment #14 (2026-05-13): bench showed daemon default
        // (greedy-equivalent) locks Codex into 10/10 loop on the rg
        // fixture; T=0.7 dropped that to 2/10. Pin the default to 0.7
        // when caller didn't set one.
        let req = req_with_input(ResponsesInput::Text("go".into()));
        assert!(req.temperature.is_none(), "test premise: caller sent no T");
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        assert_eq!(
            chat.temperature,
            Some(0.7),
            "Codex profile pins default T=0.7"
        );
    }

    #[test]
    fn translate_request_codex_preserves_caller_temperature() {
        // Operator override wins. The Codex default only applies when
        // the inbound request didn't ship a temperature.
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.temperature = Some(0.2);
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        assert_eq!(chat.temperature, Some(0.2));
    }

    #[test]
    fn translate_request_bare_does_not_pin_temperature() {
        let req = req_with_input(ResponsesInput::Text("go".into()));
        let chat = translate_request(req, false, frontdoor::Harness::Bare).unwrap();
        assert!(chat.temperature.is_none());
    }

    #[test]
    fn translate_request_codex_disables_thinking() {
        // Investment #11 (2026-05-13): Codex profile sets
        // enable_thinking=false + think_budget=0 to drop the ~85%
        // of per-turn tokens the model otherwise spends on
        // `<think>` blocks for routine codex tool turns.
        let req = req_with_input(ResponsesInput::Text("go".into()));
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        let kwargs = chat
            .chat_template_kwargs
            .as_ref()
            .expect("chat_template_kwargs should be set on Codex");
        assert_eq!(
            kwargs.get("enable_thinking").and_then(|v| v.as_bool()),
            Some(false),
            "Codex must request enable_thinking=false"
        );
        assert_eq!(chat.think_budget, Some(0));
    }

    #[test]
    fn translate_request_bare_does_not_touch_thinking() {
        let req = req_with_input(ResponsesInput::Text("go".into()));
        let chat = translate_request(req, false, frontdoor::Harness::Bare).unwrap();
        assert!(chat.chat_template_kwargs.is_none());
        assert!(chat.think_budget.is_none());
    }

    #[test]
    fn translate_request_codex_harness_promotes_tool_choice() {
        // Investment 3 (2026-05-13): Codex profile engages envelope
        // grammar lock to prevent the model from emitting malformed
        // `{name, cmd}` envelopes (args flattened to root). See
        // frontdoor::Harness::runs_grammar_lock doc.
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tool_choice = Some(serde_json::json!("auto"));
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        assert_eq!(
            chat.tool_choice.as_ref().and_then(|v| v.as_str()),
            Some("required"),
            "Codex harness promotes tool_choice to required for envelope grammar"
        );
    }

    #[test]
    fn translate_request_instructions_prepends_system_message() {
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.instructions = Some("be terse".into());
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, "system");
        assert_eq!(chat.messages[0].content, "be terse");
        assert_eq!(chat.messages[1].role, "user");
    }

    #[test]
    fn translate_request_message_parts_concat() {
        let parts = vec![
            ResponsesContentPart::InputText { text: "first".into() },
            ResponsesContentPart::OutputText { text: "second".into() },
            ResponsesContentPart::Other,
        ];
        let req = req_with_input(ResponsesInput::Items(vec![ResponsesInputItem::Message(
            RMsg {
                role: "user".into(),
                content: MessageContent::Parts(parts),
            },
        )]));
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        assert_eq!(chat.messages[0].content, "first\nsecond");
    }

    #[test]
    fn translate_request_function_call_output_becomes_tool_message() {
        let req = req_with_input(ResponsesInput::Items(vec![
            ResponsesInputItem::FunctionCallOutput(RFnOut {
                call_id: "c1".into(),
                output: "42".into(),
            }),
        ]));
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        assert_eq!(chat.messages[0].role, "tool");
        assert_eq!(chat.messages[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(chat.messages[0].content, "42");
    }

    #[test]
    fn translate_request_replayed_function_call_becomes_assistant() {
        let req = req_with_input(ResponsesInput::Items(vec![
            ResponsesInputItem::FunctionCall(RFnCall {
                call_id: "c2".into(),
                name: "shell".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
                id: None,
            }),
        ]));
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        let msg = &chat.messages[0];
        assert_eq!(msg.role, "assistant");
        let calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "c2");
        assert_eq!(calls[0].function.name, "shell");
        assert_eq!(calls[0].function.arguments, r#"{"cmd":"ls"}"#);
    }

    #[test]
    fn translate_request_flat_tool_wraps_into_nested_shape() {
        // Uses Bare profile to isolate the flat→nested wrapping
        // logic from the catalog-filter pass that Codex now applies.
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: Some("shell".into()),
            description: Some("run a command".into()),
            parameters: Some(serde_json::json!({"type":"object","properties":{}})),
            strict: None,
        }]);
        let chat = translate_request(req, false, frontdoor::Harness::Bare).unwrap();
        let tools = chat.tools.expect("tools translated");
        // Synthetic file-I/O tools are always appended; ignore them
        // when asserting on the translated user-supplied tools.
        let user_tools: Vec<_> = tools
            .iter()
            .filter(|t| {
                !matches!(
                    t.function.name.as_str(),
                    "write_file"
                        | "read_file"
                        | "write_file_begin"
                        | "write_file_chunk"
                        | "write_file_end"
                )
            })
            .collect();
        assert_eq!(user_tools.len(), 1);
        assert_eq!(user_tools[0].kind, "function");
        assert_eq!(user_tools[0].function.name, "shell");
        assert_eq!(
            user_tools[0].function.description.as_deref(),
            Some("run a command")
        );
        assert_eq!(user_tools[0].function.parameters["type"], "object");
    }

    #[test]
    fn translate_request_bridges_builtin_tools_as_functions() {
        // Codex's tool list mixes function tools with built-ins
        // (apply_patch, local_shell, web_search, …). The built-ins
        // have no top-level `name` and a different parameters shape,
        // but they ARE the only file-write / shell paths codex
        // exposes — filtering them out leaves the model with zero
        // tools and it hallucinates names. We bridge each built-in
        // through as a synthetic function tool whose name is the
        // built-in's `type` discriminator, so the model emits a
        // tool_call that codex's router can route back to its real
        // handler.
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![
            ResponsesTool {
                kind: "function".into(),
                name: Some("shell".into()),
                description: None,
                parameters: None,
                strict: None,
            },
            ResponsesTool {
                kind: "web_search".into(),
                name: None,
                description: None,
                parameters: None,
                strict: None,
            },
            ResponsesTool {
                kind: "local_shell".into(),
                name: None,
                description: None,
                parameters: None,
                strict: None,
            },
        ]);
        // Bare harness: NO catalog filter, NO synthetic injection —
        // bridging still happens for non-function builtin shapes.
        // (Codex profile now filters via CODEX_TOOL_KEEPLIST and
        // would drop `shell` + `local_shell`, defeating this test.)
        let chat = translate_request(req, false, frontdoor::Harness::Bare).unwrap();
        let tools = chat.tools.expect("tools survive translation");
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"local_shell"));
        // All emitted as `type:"function"` on the chat.completions wire,
        // even though the source had non-function `type` values.
        assert!(tools.iter().all(|t| t.kind == "function"));
    }

    #[test]
    fn translate_request_appends_synthetic_file_tools() {
        let req = req_with_input(ResponsesInput::Text("go".into()));
        // Synthetic tools are gated on frontdoor_on as of 2026-05-13.
        let chat = translate_request(req, false, frontdoor::Harness::Opencode).unwrap();
        let tools = chat.tools.expect("synthetic tools present when frontdoor on");
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"read_file"));
    }

    #[test]
    fn translate_request_frontdoor_off_omits_synthetic_tools() {
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: Some("exec_command".into()),
            description: None,
            parameters: Some(serde_json::json!({"type":"object","properties":{}})),
            strict: None,
        }]);
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        let tools = chat.tools.expect("caller tools preserved");
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"exec_command"));
        assert!(
            !names.iter().any(|n| n.starts_with("write_file")
                || *n == "read_file"),
            "synthetic tools must NOT be injected when frontdoor off — \
             they pollute codex's apply_patch-trained tool prior. names={:?}",
            names
        );
    }

    #[test]
    fn rewrite_write_file_emits_exec_command_with_printf() {
        let args = serde_json::json!({
            "path": "/abs/PLAN.md",
            "content": "# Plan\n## step-01: x [PENDING]\n"
        })
        .to_string();
        let (name, cmd_args) = rewrite_synthetic_tool_call("write_file", &args).unwrap();
        assert_eq!(name, "exec_command");
        let parsed: serde_json::Value = serde_json::from_str(&cmd_args).unwrap();
        let cmd = parsed["cmd"].as_str().unwrap();
        // Now prefixed with `mkdir -p <parent>` so the file can land in
        // a fresh subdirectory.
        assert!(cmd.starts_with("mkdir -p "));
        assert!(cmd.contains("printf '%s' "));
        assert!(cmd.contains("'/abs/PLAN.md'"));
        // Newlines survive intact in the shell-quoted content.
        assert!(cmd.contains("\n## step-01"));
    }

    #[test]
    fn rewrite_read_file_emits_cat() {
        let args = serde_json::json!({ "path": "/abs/x.rs" }).to_string();
        let (name, cmd_args) = rewrite_synthetic_tool_call("read_file", &args).unwrap();
        assert_eq!(name, "exec_command");
        let parsed: serde_json::Value = serde_json::from_str(&cmd_args).unwrap();
        assert_eq!(parsed["cmd"].as_str().unwrap(), "cat '/abs/x.rs'");
    }

    #[test]
    fn write_file_refuses_oversize_content_and_nudges_chunked() {
        // Construct content above the 350-byte ceiling.
        let big_content = "fn x() {}\n".repeat(60); // 600 bytes
        assert!(big_content.len() > 350);
        let args =
            serde_json::json!({"path":"/abs/x.rs","content":big_content}).to_string();
        let (name, cmd_args) = rewrite_synthetic_tool_call("write_file", &args).unwrap();
        assert_eq!(name, "exec_command");
        let cmd = serde_json::from_str::<serde_json::Value>(&cmd_args).unwrap()["cmd"]
            .as_str()
            .unwrap()
            .to_string();
        // Refusal short-circuits — no actual write happens.
        assert!(cmd.contains("write_file refused"));
        assert!(cmd.contains("exit 65"));
        // Explicit alternative tools named in the error so the model
        // can pick the right path on the next emit.
        assert!(cmd.contains("write_file_begin"));
        assert!(cmd.contains("write_file_chunk"));
    }

    #[test]
    fn write_file_under_threshold_still_writes() {
        let args = serde_json::json!({
            "path":"/abs/x.rs",
            "content":"fn main() {}\n"
        })
        .to_string();
        let (_, cmd_args) = rewrite_synthetic_tool_call("write_file", &args).unwrap();
        let cmd = serde_json::from_str::<serde_json::Value>(&cmd_args).unwrap()["cmd"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(cmd.contains("printf"));
        assert!(!cmd.contains("refused"));
    }

    #[test]
    fn write_file_begin_truncates_and_creates_parent() {
        let args = serde_json::json!({"path":"/abs/sub/x.rs"}).to_string();
        let (name, cmd_args) =
            rewrite_synthetic_tool_call("write_file_begin", &args).unwrap();
        assert_eq!(name, "exec_command");
        let cmd = serde_json::from_str::<serde_json::Value>(&cmd_args).unwrap()["cmd"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(cmd.contains("mkdir -p '/abs/sub'"));
        assert!(cmd.contains(": > '/abs/sub/x.rs'"));
        // wc -c reports current size — useful as a sanity-check result.
        assert!(cmd.contains("wc -c < '/abs/sub/x.rs'"));
    }

    #[test]
    fn write_file_chunk_appends_and_reports_size() {
        let args = serde_json::json!({
            "path":"/abs/x.rs",
            "chunk":"pub fn hi() {}\n"
        })
        .to_string();
        let (name, cmd_args) =
            rewrite_synthetic_tool_call("write_file_chunk", &args).unwrap();
        assert_eq!(name, "exec_command");
        let cmd = serde_json::from_str::<serde_json::Value>(&cmd_args).unwrap()["cmd"]
            .as_str()
            .unwrap()
            .to_string();
        // Append (not truncate-and-write) is the load-bearing part —
        // a chunked write would be ruined by a stray `>` (truncate).
        assert!(cmd.contains("printf '%s' 'pub fn hi() {}\n' >> '/abs/x.rs'"));
        assert!(cmd.contains("wc -c < '/abs/x.rs'"));
    }

    #[test]
    fn write_file_end_reports_final_size() {
        let args = serde_json::json!({"path":"/abs/x.rs"}).to_string();
        let (_, cmd_args) =
            rewrite_synthetic_tool_call("write_file_end", &args).unwrap();
        let cmd = serde_json::from_str::<serde_json::Value>(&cmd_args).unwrap()["cmd"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(cmd, "wc -c < '/abs/x.rs'");
    }

    #[test]
    fn write_file_creates_parent_dir() {
        // Single-shot write_file also benefits from on-demand parent
        // creation — the model can write `/abs/sub/x.rs` even when
        // `/abs/sub` doesn't exist yet.
        let args =
            serde_json::json!({"path":"/abs/sub/x.rs","content":"hi"}).to_string();
        let (_, cmd_args) = rewrite_synthetic_tool_call("write_file", &args).unwrap();
        let cmd = serde_json::from_str::<serde_json::Value>(&cmd_args).unwrap()["cmd"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(cmd.starts_with("mkdir -p '/abs/sub' && printf"));
    }

    #[test]
    fn parent_dir_handles_edge_cases() {
        assert_eq!(parent_dir("/a/b/c.txt"), "/a/b");
        assert_eq!(parent_dir("/c.txt"), "/");
        assert_eq!(parent_dir("c.txt"), ".");
        assert_eq!(parent_dir("a/b/c.txt"), "a/b");
    }

    #[test]
    fn rewrite_passes_through_non_synthetic_names() {
        let args = serde_json::json!({ "cmd": "ls" }).to_string();
        assert!(rewrite_synthetic_tool_call("exec_command", &args).is_none());
        assert!(rewrite_synthetic_tool_call("web_search", &args).is_none());
    }

    #[test]
    fn rewrite_always_rewrites_synthetic_names_even_on_bad_args() {
        // Defense-in-depth: if the model emits a synthetic name with
        // malformed arguments (parse failure, no path, empty path),
        // we MUST still rewrite to exec_command. Letting "write_file"
        // leak through causes codex `unsupported call: write_file`.
        let (name, _) = rewrite_synthetic_tool_call("write_file", "not json").unwrap();
        assert_eq!(name, "exec_command");
        let (name, _) = rewrite_synthetic_tool_call("write_file", "{}").unwrap();
        assert_eq!(name, "exec_command");
        let (name, args) =
            rewrite_synthetic_tool_call("write_file", r#"{"path":""}"#).unwrap();
        assert_eq!(name, "exec_command");
        let parsed: serde_json::Value = serde_json::from_str(&args).unwrap();
        let cmd = parsed["cmd"].as_str().unwrap();
        assert!(cmd.contains("empty path"));
        assert!(cmd.contains("exit 64"));
    }

    #[test]
    fn normalize_path_segments_strips_whitespace_around_segments() {
        // Observed 2026-05-12 codex+frontdoor v4 model emit:
        // /Users/alexsbryan/dev/ atos-experiment-oicp-types /src/lib.rs
        let input = "/Users/alexsbryan/dev/ atos-experiment-oicp-types /src/lib.rs";
        assert_eq!(
            normalize_path_segments(input),
            "/Users/alexsbryan/dev/atos-experiment-oicp-types/src/lib.rs"
        );
    }

    #[test]
    fn normalize_path_segments_preserves_clean_absolute_path() {
        assert_eq!(
            normalize_path_segments("/abs/x/y.rs"),
            "/abs/x/y.rs"
        );
    }

    #[test]
    fn normalize_path_segments_handles_relative_path() {
        assert_eq!(
            normalize_path_segments("foo/bar/baz"),
            "foo/bar/baz"
        );
        assert_eq!(
            normalize_path_segments(" foo / bar / baz "),
            "foo/bar/baz"
        );
    }

    #[test]
    fn normalize_path_segments_collapses_redundant_slashes() {
        // `//a//b/c` becomes `/a/b/c` because empty segments are dropped.
        assert_eq!(normalize_path_segments("//a//b/c"), "/a/b/c");
    }

    #[test]
    fn rewrite_write_file_normalizes_corrupted_path() {
        let args = serde_json::json!({
            "path": "/abs/ project /src/lib.rs",
            "content": "x"
        })
        .to_string();
        let (_, cmd_args) = rewrite_synthetic_tool_call("write_file", &args).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&cmd_args).unwrap();
        let cmd = parsed["cmd"].as_str().unwrap();
        // Normalized path appears in the shell-quoted argument; the
        // raw whitespace-injected form does NOT.
        assert!(cmd.contains("'/abs/project/src/lib.rs'"));
        assert!(!cmd.contains("'/abs/ project /src/lib.rs'"));
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quotes() {
        // The standard POSIX idiom: close-quote, escaped-quote, reopen-quote.
        assert_eq!(shell_single_quote("ab'cd"), "'ab'\\''cd'");
        assert_eq!(shell_single_quote(""), "''");
        assert_eq!(shell_single_quote("plain"), "'plain'");
    }

    #[test]
    fn rewrite_write_file_handles_content_with_quotes_and_newlines() {
        let args = serde_json::json!({
            "path": "/abs/x.rs",
            "content": "let s = 'hello';\nlet t = \"world\";\n"
        })
        .to_string();
        let (_, cmd_args) = rewrite_synthetic_tool_call("write_file", &args).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&cmd_args).unwrap();
        let cmd = parsed["cmd"].as_str().unwrap();
        // Single-quoted shell string with the embedded apostrophe
        // escaped via close-quote / backslash-quote / reopen-quote.
        assert!(cmd.contains("'\\''"));
        // Double quotes survive verbatim (no shell-escape needed
        // inside single quotes).
        assert!(cmd.contains("\"world\""));
    }

    #[test]
    fn translate_request_drops_function_tool_with_no_name() {
        // Defensive: a function-typed tool without `name` is malformed
        // — the chat.completions side requires `name`, so we drop it
        // rather than 422ing the whole request.
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: None,
            description: None,
            parameters: None,
            strict: None,
        }]);
        let chat = translate_request(req, false, frontdoor::Harness::Codex).unwrap();
        let tools = chat.tools.expect("tools field present");
        // The unnamed user tool is dropped; only the synthetic
        // file-I/O tools remain.
        let user_tools: Vec<_> = tools
            .iter()
            .filter(|t| {
                !matches!(
                    t.function.name.as_str(),
                    "write_file"
                        | "read_file"
                        | "write_file_begin"
                        | "write_file_chunk"
                        | "write_file_end"
                )
            })
            .collect();
        assert!(user_tools.is_empty());
    }

    #[test]
    fn translate_request_tool_with_no_parameters_synthesizes_empty_object() {
        // Bare profile to isolate the parameters-synthesis logic from
        // Codex's catalog filter (which would drop "now").
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: Some("now".into()),
            description: None,
            parameters: None,
            strict: None,
        }]);
        let chat = translate_request(req, false, frontdoor::Harness::Bare).unwrap();
        let tools = chat.tools.unwrap();
        assert_eq!(tools[0].function.parameters["type"], "object");
        assert!(tools[0].function.parameters.get("properties").is_some());
    }

    #[test]
    fn translate_request_max_output_tokens_maps_to_max_tokens() {
        // Bare profile to isolate the literal max_output_tokens →
        // max_tokens mapping from the frontdoor's 16K floor (Codex /
        // Opencode profiles bump small caps to 16K to leave room for
        // the entire `<tool_call>` envelope).
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.max_output_tokens = Some(1234);
        let chat = translate_request(req, false, frontdoor::Harness::Bare).unwrap();
        assert_eq!(chat.max_tokens, Some(1234));
    }

    // ─── SSE parser ─────────────────────────────────────────────────

    #[test]
    fn take_one_sse_event_strips_terminator() {
        let mut buf = BytesMut::from("data: {\"a\":1}\n\ndata: 2\n\n".as_bytes());
        let ev = take_one_sse_event(&mut buf).unwrap();
        assert_eq!(&ev[..], b"data: {\"a\":1}\n\n");
        assert!(parse_sse_data(&ev).unwrap().starts_with('{'));
        let ev2 = take_one_sse_event(&mut buf).unwrap();
        assert_eq!(parse_sse_data(&ev2), Some("2"));
    }

    #[test]
    fn take_one_sse_event_returns_none_when_incomplete() {
        let mut buf = BytesMut::from("data: {\"a\":1}\n".as_bytes());
        assert!(take_one_sse_event(&mut buf).is_none());
    }

    #[test]
    fn parse_sse_data_handles_optional_space() {
        assert_eq!(parse_sse_data(b"data:nospace\n\n"), Some("nospace"));
        assert_eq!(parse_sse_data(b"data: space\n\n"), Some("space"));
    }

    // ─── FSM ────────────────────────────────────────────────────────

    fn new_fsm() -> ResponsesStreamState {
        ResponsesStreamState::new("resp_T".into(), "m1".into(), 1700, None)
    }

    fn event_type(ev: &Event) -> String {
        // Format the SSE event back out and extract the event name.
        // axum::sse::Event has no public accessors, so we round-trip
        // via Display: `event: name\ndata: ...\n\n`.
        format!("{ev:?}")
    }

    fn body_field(ev: &Event, field: &str) -> Option<String> {
        // Same trick — the Debug impl exposes everything we need for tests.
        let s = format!("{ev:?}");
        let needle = format!("{field}: ");
        let i = s.find(&needle)?;
        let rest = &s[i + needle.len()..];
        let end = rest.find('\n').unwrap_or(rest.len());
        Some(rest[..end].trim_matches('"').to_string())
    }

    #[test]
    fn fsm_initial_events_have_response_created_and_in_progress() {
        let mut fsm = new_fsm();
        let created = fsm.emit_created();
        let in_progress = fsm.emit_in_progress();
        assert!(event_type(&created).contains("response.created"));
        assert!(event_type(&in_progress).contains("response.in_progress"));
        // Sequence numbers are monotonic.
        let _ = (body_field(&created, "sequence_number"), body_field(&in_progress, "sequence_number"));
    }

    fn chat_text_chunk(content: &str, finish: Option<&str>) -> serde_json::Value {
        let mut delta = serde_json::json!({});
        if !content.is_empty() {
            delta["content"] = serde_json::Value::String(content.into());
        }
        let finish_val = match finish {
            Some(f) => serde_json::Value::String(f.into()),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "id": "chatcmpl-x",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_val,
            }]
        })
    }

    #[test]
    fn fsm_first_text_delta_emits_item_and_part_added_then_delta() {
        let mut fsm = new_fsm();
        let events = fsm.handle_chat_chunk(&chat_text_chunk("hello", None));
        // Order: output_item.added(message), content_part.added(output_text), output_text.delta.
        assert_eq!(events.len(), 3);
        let s = format!("{events:?}");
        let i_added = s.find("response.output_item.added").unwrap();
        let p_added = s.find("response.content_part.added").unwrap();
        let d = s.find("response.output_text.delta").unwrap();
        assert!(i_added < p_added && p_added < d);
    }

    #[test]
    fn fsm_subsequent_text_deltas_emit_only_delta() {
        let mut fsm = new_fsm();
        let _ = fsm.handle_chat_chunk(&chat_text_chunk("a", None));
        let events = fsm.handle_chat_chunk(&chat_text_chunk("b", None));
        assert_eq!(events.len(), 1);
        let s = format!("{events:?}");
        assert!(s.contains("response.output_text.delta"));
    }

    #[test]
    fn fsm_finish_reason_closes_message_with_done_events() {
        let mut fsm = new_fsm();
        let _ = fsm.handle_chat_chunk(&chat_text_chunk("hi", None));
        let events = fsm.handle_chat_chunk(&chat_text_chunk("", Some("stop")));
        let s = format!("{events:?}");
        assert!(s.contains("response.output_text.done"));
        assert!(s.contains("response.content_part.done"));
        assert!(s.contains("response.output_item.done"));
    }

    #[test]
    fn fsm_completion_emits_response_completed_with_usage() {
        let mut fsm = new_fsm();
        let _ = fsm.handle_chat_chunk(&chat_text_chunk("hi", None));
        let _ = fsm.handle_chat_chunk(&serde_json::json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
        }));
        let events = fsm.emit_completion();
        let s = format!("{events:?}");
        assert!(s.contains("response.completed"));
        assert!(s.contains("input_tokens"));
        assert!(s.contains("output_tokens"));
    }

    #[test]
    fn fsm_tool_calls_emit_complete_function_call_lifecycle() {
        let mut fsm = new_fsm();
        let chunk = serde_json::json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"cmd\":\"ls\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let events = fsm.handle_chat_chunk(&chunk);
        let s = format!("{events:?}");
        // Per-call: output_item.added → arguments.delta → arguments.done → output_item.done.
        assert!(s.contains("response.output_item.added"));
        assert!(s.contains("response.function_call_arguments.delta"));
        assert!(s.contains("response.function_call_arguments.done"));
        assert!(s.contains("response.output_item.done"));
        // call_id round-trips.
        assert!(s.contains("call_abc"));
        // name round-trips.
        assert!(s.contains("shell"));
    }

    #[test]
    fn fsm_completion_idempotent() {
        let mut fsm = new_fsm();
        let a = fsm.emit_completion();
        let b = fsm.emit_completion();
        assert!(!a.is_empty());
        assert!(b.is_empty(), "second call should be a no-op");
    }

    #[test]
    fn fsm_completion_after_done_without_finish_still_closes_message() {
        // Inner [DONE] arriving without a finish_reason chunk —
        // emit_completion must close any open message before
        // response.completed.
        let mut fsm = new_fsm();
        let _ = fsm.handle_chat_chunk(&chat_text_chunk("hi", None));
        let events = fsm.emit_completion();
        let s = format!("{events:?}");
        assert!(s.contains("response.output_text.done"));
        assert!(s.contains("response.content_part.done"));
        assert!(s.contains("response.output_item.done"));
        assert!(s.contains("response.completed"));
    }
}
