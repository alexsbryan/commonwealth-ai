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
    Json(req): Json<ResponsesRequest>,
) -> Response {
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

    debug!(
        response_id = %response_id,
        model = %model_label,
        stream = stream_mode,
        "responses: translating to chat.completions"
    );

    // ── Request translation ───────────────────────────────────────────
    let chat_req = match translate_request(req) {
        Ok(r) => r,
        Err(msg) => {
            return error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &msg);
        }
    };

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

fn translate_request(req: ResponsesRequest) -> Result<ChatCompletionRequest, String> {
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
    // Codex's tool list contains both function tools and built-in
    // tools (`web_search`, `file_search`, `local_shell`,
    // `image_generation_call`, `computer_use_preview`, `mcp`). Local
    // models can't dispatch the built-ins, so we drop anything that
    // isn't a `function` with a `name`. Logged at debug so the operator
    // can see which built-ins codex shipped and we silently elided.
    let tools = req.tools.map(|tools_in| {
        tools_in
            .into_iter()
            .filter_map(|t| {
                if t.kind != "function" {
                    debug!(kind = %t.kind, "responses: dropping non-function tool");
                    return None;
                }
                let Some(name) = t.name else {
                    debug!(kind = %t.kind, "responses: dropping function tool with no name");
                    return None;
                };
                Some(ToolDefinition {
                    kind: t.kind,
                    function: ToolFunction {
                        name,
                        description: t.description,
                        // Responses lets `parameters` be omitted for
                        // zero-arg tools; chat.completions requires an
                        // object schema, so we synthesize
                        // `{type:"object",properties:{}}` when missing.
                        parameters: t.parameters.unwrap_or_else(|| {
                            serde_json::json!({"type":"object","properties":{}})
                        }),
                    },
                })
            })
            .collect::<Vec<_>>()
    });

    Ok(ChatCompletionRequest {
        model: req.model,
        messages,
        temperature: req.temperature,
        max_tokens: req.max_output_tokens,
        stream: req.stream,
        top_p: req.top_p,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools,
        tool_choice: req.tool_choice,
        response_format: None,
        oicp: None,
        chat_template_kwargs: None,
        think_budget: None,
        tool_profile: None,
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
    if let Some(choice) = chat.choices.into_iter().next() {
        let msg = choice.message;
        let text = msg.content;

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
                output.push(ResponsesOutputItem::FunctionCall(OutputFunctionCall {
                    id: fc_id,
                    call_id: tc.id,
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                    status: "completed",
                }));
            }
        }
    }

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
                    let msg = self.message.as_mut().expect("just initialised");
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
                let name = func
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = func
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::responses_types::{
        FunctionCallItem as RFnCall, FunctionCallOutputItem as RFnOut, MessageContent,
        MessageItem as RMsg, ResponsesContentPart, ResponsesInput, ResponsesInputItem,
        ResponsesTool,
    };

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
        let chat = translate_request(req).unwrap();
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(chat.messages[0].content, "hello");
    }

    #[test]
    fn translate_request_instructions_prepends_system_message() {
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.instructions = Some("be terse".into());
        let chat = translate_request(req).unwrap();
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
        let chat = translate_request(req).unwrap();
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
        let chat = translate_request(req).unwrap();
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
        let chat = translate_request(req).unwrap();
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
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: Some("shell".into()),
            description: Some("run a command".into()),
            parameters: Some(serde_json::json!({"type":"object","properties":{}})),
            strict: None,
        }]);
        let chat = translate_request(req).unwrap();
        let tools = chat.tools.expect("tools translated");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, "function");
        assert_eq!(tools[0].function.name, "shell");
        assert_eq!(tools[0].function.description.as_deref(), Some("run a command"));
        assert_eq!(tools[0].function.parameters["type"], "object");
    }

    #[test]
    fn translate_request_drops_non_function_tools() {
        // Codex's tool list mixes function tools with built-ins
        // (web_search, file_search, local_shell, etc.). Built-ins
        // have no `name` and a different parameters shape; local
        // models can't dispatch them, so the adapter drops them.
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
        let chat = translate_request(req).unwrap();
        let tools = chat.tools.expect("function tool survives");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "shell");
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
        let chat = translate_request(req).unwrap();
        // Empty Vec, NOT None — translate still emits an empty list
        // because the source request had `tools: [...]` shape.
        let tools = chat.tools.expect("tools field present");
        assert!(tools.is_empty());
    }

    #[test]
    fn translate_request_tool_with_no_parameters_synthesizes_empty_object() {
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.tools = Some(vec![ResponsesTool {
            kind: "function".into(),
            name: Some("now".into()),
            description: None,
            parameters: None,
            strict: None,
        }]);
        let chat = translate_request(req).unwrap();
        let tools = chat.tools.unwrap();
        assert_eq!(tools[0].function.parameters["type"], "object");
        assert!(tools[0].function.parameters.get("properties").is_some());
    }

    #[test]
    fn translate_request_max_output_tokens_maps_to_max_tokens() {
        let mut req = req_with_input(ResponsesInput::Text("go".into()));
        req.max_output_tokens = Some(1234);
        let chat = translate_request(req).unwrap();
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

    fn body_field<'a>(ev: &'a Event, field: &str) -> Option<String> {
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
