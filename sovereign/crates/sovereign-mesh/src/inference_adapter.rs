//! Adapter: `sovereign_core::traits::InferenceProvider` →
//! `commonwealth_api::state::LocalInferenceService`.
//!
//! Why this exists: Commonwealth's HTTP handlers speak OpenAI-style
//! `ChatCompletionRequest`/`Response`; Sovereign's runtime speaks
//! its own `CompletionRequest`/`Response`. The two are similar but
//! not identical — messages-vs-prompt, sampling knob names, OICP
//! shape. This adapter owns the translation in one place so neither
//! side has to know about the other.
//!
//! Scope for v2: non-streaming tool-call round-trip against the Slow
//! slot. `request.tools` is injected into the model's chat template
//! by `sovereign-inference::embedded::format_prompt`; the raw model
//! output is parsed by `parse_tool_calls_with_errors` and the
//! resulting structured `tool_calls` populated on the response.
//! Streaming with tools is deferred (non-streaming is forced when
//! `tools.is_some()`).
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_api::openai_types::{
    self as wire, ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage,
    FunctionCall, Role, ToolCall, Usage,
};
use commonwealth_api::state::LocalInferenceService;
use commonwealth_inference::oicp::ProviderManifest;
use futures::{Stream, StreamExt};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, FinishReason as CoreFinishReason,
    StreamFrame as CoreStreamFrame, StreamUsage as CoreStreamUsage,
};

/// Translate `sovereign_core` stream framing into the wire shape
/// `LocalInferenceService::chat_completion_stream` exposes. The two
/// enums are identical by design (see `openai_types::StreamFrame`),
/// so this is a pure per-variant copy with no data loss.
fn translate_stream_frame(frame: CoreStreamFrame) -> wire::StreamFrame {
    match frame {
        CoreStreamFrame::Token(text) => wire::StreamFrame::Token(text),
        CoreStreamFrame::Finish { reason, usage } => wire::StreamFrame::Finish {
            reason: translate_finish_reason(reason),
            usage: usage.map(translate_stream_usage),
        },
        CoreStreamFrame::Error(msg) => {
            // Stream errors are unusual and load-bearing — they
            // terminate the request from the model side. Trace once
            // per occurrence (cheap: error frames are rare by design).
            tracing::warn!(error = %msg, "inference_adapter:stream_error_frame");
            wire::StreamFrame::Error(msg)
        }
    }
}

fn translate_finish_reason(r: CoreFinishReason) -> wire::FinishReason {
    // Non-Stop reasons are operationally interesting (Length =
    // truncation, ContentFilter = guard tripped, Cancelled = client
    // bailed, Error = backend failed). Stop is the silent default.
    match r {
        CoreFinishReason::Stop => wire::FinishReason::Stop,
        CoreFinishReason::Length => {
            tracing::debug!("inference_adapter:finish_reason_length");
            wire::FinishReason::Length
        }
        CoreFinishReason::ToolCalls => {
            tracing::debug!("inference_adapter:finish_reason_tool_calls");
            wire::FinishReason::ToolCalls
        }
        CoreFinishReason::ContentFilter => {
            tracing::warn!("inference_adapter:finish_reason_content_filter");
            wire::FinishReason::ContentFilter
        }
        CoreFinishReason::Cancelled => {
            tracing::debug!("inference_adapter:finish_reason_cancelled");
            wire::FinishReason::Cancelled
        }
        CoreFinishReason::Error(msg) => {
            tracing::warn!(error = %msg, "inference_adapter:finish_reason_error");
            wire::FinishReason::Error(msg)
        }
    }
}

fn translate_stream_usage(u: CoreStreamUsage) -> wire::StreamUsage {
    wire::StreamUsage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    }
}

/// Convert a fully-buffered `ChatCompletionResponse` into a sequence
/// of stream frames that, when serialised as OpenAI SSE chunks,
/// honour the streaming-with-tools wire shape clients (opencode,
/// `openai-python`) expect:
///
/// 1. Optional `Token(content)` — only when the assistant message has
///    non-empty prose alongside the tool calls. Most tool-only turns
///    skip this entirely.
/// 2. `ToolCalls(...)` — the full set of parsed tool calls in one
///    frame. `routes_inference::serve_local_stream` renders this with
///    `delta.role = "assistant"` and `delta.tool_calls = [...]` per
///    OpenAI's chunk schema.
/// 3. `Finish { reason, usage }` — terminal frame, reason is whatever
///    the non-streaming path settled on (`tool_calls` when calls were
///    parsed, `stop` otherwise).
///
/// Trade-off: this loses token-by-token streaming for tool turns
/// (the parser only finds `<tool_call>` markup once generation
/// completes). For ATOS-style agentic workflows that's acceptable —
/// tool turns are short bursts, and the round-trip clients care about
/// is the structured tool_call payload, not the keystroke cadence.
fn synthesize_tool_stream(
    resp: ChatCompletionResponse,
) -> Vec<wire::StreamFrame> {
    let mut frames: Vec<wire::StreamFrame> = Vec::with_capacity(3);

    let choice = resp.choices.into_iter().next();
    let choice_present = choice.is_some();
    let (content, tool_calls, finish_reason_str) = match choice {
        Some(c) => (
            c.message.content,
            c.message.tool_calls.unwrap_or_default(),
            c.finish_reason.unwrap_or_else(|| "stop".into()),
        ),
        None => (String::new(), Vec::new(), "stop".into()),
    };
    if !choice_present {
        // Provider returned a response with no choices — degenerate
        // case that downstream clients receive as an empty stream.
        // Loud so it surfaces in incident-triage logs.
        tracing::warn!("inference_adapter:synthesize_tool_stream_no_choice");
    }

    let has_content = !content.trim().is_empty();
    let has_tool_calls = !tool_calls.is_empty();

    if has_content {
        frames.push(wire::StreamFrame::Token(content));
    }
    if has_tool_calls {
        frames.push(wire::StreamFrame::ToolCalls(tool_calls.clone()));
    }
    // Classify the synthesis shape so operators can post-hoc tell
    // which path the model took without grepping individual frames.
    // Four observed shapes: tool-only (most common for agentic
    // turns), content-only (fallback when grammar fails), mixed,
    // and empty (degenerate — already warned above).
    let shape = match (has_content, has_tool_calls) {
        (false, true) => "tool_only",
        (true, true) => "content_plus_tools",
        (true, false) => "content_only",
        (false, false) => "empty",
    };
    tracing::debug!(
        shape = %shape,
        tool_call_count = tool_calls.len(),
        finish_reason = %finish_reason_str,
        "inference_adapter:synthesize_tool_stream_shape"
    );

    let parsed_reason = wire::FinishReason::from_openai_str(&finish_reason_str);
    // Unknown finish-reason strings collapse to Stop inside
    // `from_openai_str`; leave a breadcrumb when the upstream value
    // wasn't recognised so a vocabulary regression surfaces in
    // telemetry. "stop" is the silent default.
    if matches!(parsed_reason, wire::FinishReason::Stop)
        && finish_reason_str != "stop"
    {
        tracing::debug!(
            finish_reason = %finish_reason_str,
            "inference_adapter:synthesize_tool_stream_unknown_finish_reason"
        );
    }
    let reason = parsed_reason;
    let usage = resp.usage.map(|u| wire::StreamUsage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });
    frames.push(wire::StreamFrame::Finish { reason, usage });
    frames
}

/// Wraps a sovereign-core `InferenceProvider` so it can answer
/// Commonwealth-flavoured `/v1/chat/completions` requests from
/// peers on the mesh. Held inside an `Arc` — cheap to clone,
/// thread-safe.
pub struct SovereignInferenceAdapter {
    provider: Arc<dyn InferenceProvider>,
}

impl SovereignInferenceAdapter {
    pub fn new(provider: Arc<dyn InferenceProvider>) -> Self {
        Self { provider }
    }

    /// Flatten an OpenAI-style message list into a single prompt.
    /// Sovereign's `CompletionRequest` takes a flat prompt plus an
    /// optional system message, so we concat user/assistant turns
    /// into the prompt and pull the first system message out
    /// separately. Assistant replies in the history become labelled
    /// `Assistant:` lines — gives the model conversational context
    /// without needing a proper chat template layer here.
    fn flatten(request: &ChatCompletionRequest) -> (String, Option<String>) {
        let mut system: Option<String> = None;
        let mut convo = String::new();
        for msg in &request.messages {
            match Role::from_openai_str(msg.role.as_str()) {
                Role::System => {
                    if system.is_none() {
                        system = Some(msg.content.clone());
                    } else if let Some(existing) = system.as_mut() {
                        // Multiple system messages — keep them all,
                        // separated. Rare but allowed by OpenAI.
                        existing.push_str("\n\n");
                        existing.push_str(&msg.content);
                    }
                }
                Role::User => {
                    convo.push_str("User: ");
                    convo.push_str(&msg.content);
                    convo.push_str("\n\n");
                }
                Role::Assistant => {
                    convo.push_str("Assistant: ");
                    convo.push_str(&msg.content);
                    // Replay prior tool-call requests back into the
                    // prompt in the same `<tool_call>{json}</tool_call>`
                    // format the model emits. This keeps the agent loop
                    // coherent when a client resends an entire
                    // conversation history that includes earlier tool
                    // invocations — the model sees its own prior calls
                    // instead of a silent gap.
                    if let Some(calls) = msg.tool_calls.as_ref() {
                        for tc in calls {
                            let entry = serde_json::json!({
                                "name": tc.function.name,
                                "arguments": tc.function.arguments,
                            });
                            convo.push_str("<tool_call>");
                            convo.push_str(&entry.to_string());
                            convo.push_str("</tool_call>\n");
                        }
                    }
                    convo.push_str("\n");
                }
                Role::Tool => {
                    // Tool-result turn. Tag the content with the
                    // call id so a downstream template layer (or the
                    // model, for models that parse it) can correlate
                    // the result with the originating `<tool_call>`.
                    // Keeping this stringified rather than dropping
                    // the id means a malformed replay is visible —
                    // silent drops would masquerade as "tool
                    // returned nothing".
                    if let Some(id) = msg.tool_call_id.as_deref() {
                        convo.push_str(&format!("Tool[{id}]: "));
                    } else {
                        convo.push_str("Tool: ");
                    }
                    convo.push_str(&msg.content);
                    convo.push_str("\n\n");
                }
                Role::Other(name) => {
                    // Other roles (function, developer, …) go through
                    // verbatim as labelled lines — harmless for models
                    // that don't understand them.
                    convo.push_str(&name);
                    convo.push_str(": ");
                    convo.push_str(&msg.content);
                    convo.push_str("\n\n");
                }
            }
        }
        convo.push_str("Assistant:");
        (convo, system)
    }

    /// Translate OpenAI tool defs into Sovereign core `ToolSchema`.
    /// Kept as a separate helper so the adapter stays testable without
    /// needing a live inference provider.
    fn forward_tools(request: &ChatCompletionRequest) -> Option<Vec<sovereign_core::types::ToolSchema>> {
        let tools = request.tools.as_ref()?;
        if tools.is_empty() {
            return None;
        }
        tracing::debug!(
            tool_count = tools.len(),
            "inference_adapter:forward_tools"
        );
        Some(
            tools
                .iter()
                .map(|t| sovereign_core::types::ToolSchema {
                    name: t.function.name.clone(),
                    description: t.function.description.clone(),
                    parameters: t.function.parameters.clone(),
                })
                .collect(),
        )
    }

    /// Build the internal `CompletionRequest` a peer-served chat
    /// completion should carry. The slot (Fast vs Slow) is chosen
    /// by re-applying OICP scoring against our local manifest —
    /// see `oicp_select::pick_slot_for_oicp`. Without this, the
    /// Joiner's OICP selection work stops at our front door: we
    /// default to Slow and fire up the primary slot regardless of
    /// whether a smaller, faster slot would already satisfy the
    /// request's capability requirements.
    fn build_completion_request(
        &self,
        request: &ChatCompletionRequest,
    ) -> CompletionRequest {
        let (prompt, system) = Self::flatten(request);
        // Build a skeleton request with the OICP envelope attached
        // BEFORE choosing a slot — the slot picker reads the
        // envelope to decide.
        let mut req = CompletionRequest::new(&prompt);
        if let Some(s) = system {
            req = req.with_system(&s);
        }
        // Forward the OpenAI `model` field as `model_id` so the
        // local provider's slot picker can route to a named slot
        // when one is loaded. Empty/whitespace strings stay None so
        // the slot picker falls through to its default policy. The
        // OpenAI `model` field is `Option<String>` because some
        // clients omit it for legacy completions endpoints.
        if let Some(model) = request.model.as_ref() {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                req = req.with_model_id(trimmed);
            }
        }
        req.max_tokens = request.max_tokens.map(|n| n as usize);
        req.temperature = request.temperature;
        req.top_p = request.top_p;
        req.sampling_mode = request.sampling_mode;
        req.assistant_prefix = request.assistant_prefix.clone();
        req.cmd_prefix = request.cmd_prefix.clone();
        req.url_allowlist = request.url_allowlist.clone();
        req.evidence_id_allowlist = request.evidence_id_allowlist.clone();
        // Forward the Commonwealth `think_budget` extension. The
        // daemon's `format_prompt` reads `req.think_budget == Some(0)`
        // to inject `/no_think` for SystemPromptToken thinking
        // families (Qwen3 / Qwen3.5 / SmolLM3). Schema-constrained
        // structured-output callers (atlas Phase 1) typically set
        // this so the model spends every output token on the JSON
        // payload rather than a chain-of-thought.
        req.think_budget = resolve_think_budget(request);
        if let Some(oicp) = &request.oicp {
            req = req.with_oicp(oicp.clone());
        }
        // Tool-use: carry tool schemas + tool_choice through so the
        // inference backend can inject them into the chat template
        // (`sovereign-inference::embedded::format_prompt`). When tools
        // are present we also bias the slot picker toward Slow — the
        // policy guard (`guard_tools_on_fast`) rejects Fast+tools
        // later in the pipeline, so biasing here avoids racing into a
        // guaranteed-reject state.
        req.tools = Self::forward_tools(request);
        req.tool_choice = request.tool_choice.clone();
        // OpenAI `response_format: {"type":"json_schema", json_schema:
        // {"name":..., "schema":..., "strict":...}}` → core
        // `structured_output: <schema>`. The sampler builder
        // (`build_sampler` in sovereign-inference::embedded) consumes
        // this as the JSON Schema for `LlamaSampler::llguidance`. We
        // also accept `{"type":"json_object"}` (any-JSON) by mapping
        // to the trivial `{"type":"object"}` schema.
        if let Some(rf) = request.response_format.as_ref() {
            req.structured_output = extract_response_format_schema(rf);
        }
        // Tool-call grammar: when the caller sets `tool_choice =
        // "required"` (OpenAI semantics: model MUST call a tool) and
        // tools are present, install a JSON-Schema grammar over the
        // tool envelope. The sampler then masks out any token that
        // would lead to an invalid `{"name": <tool_name>, "arguments":
        // <tool_params>}` body. Closes the three observed long-emit
        // failure modes at the source: FINAL-Bench dropping the
        // closing brace, Qwen-Coder emitting raw `\n` inside string
        // values, and character-drop-in-Rust corruption of edits
        // (because the grammar forbids any token that breaks the
        // schema, including ones that would make `arguments`
        // malformed). Caller-set response_format wins — we only
        // populate when nothing else has.
        //
        // `SOVEREIGN_FORCE_TOOL_CALLS=1` env-var override:
        // tools-using clients that don't pass tool_choice (opencode,
        // Aider, the Anthropic SDK's openai-compat shim, …) can opt
        // into the grammar by setting this env var on the daemon.
        // When set, any request with non-empty tools is treated as
        // if `tool_choice = "required"` for grammar-installation
        // purposes. The original tool_choice is left intact on `req`
        // so downstream tooling (chat-template selection,
        // tools-on-fast-slot guard, telemetry) sees what the caller
        // actually sent.
        if req.structured_output.is_none() && request.assistant_prefix.is_none() {
            // R1 (assistant_prefix) and envelope-schema don't compose:
            // the prefill places the model mid-JSON while the grammar
            // mask starts fresh on the first generated token expecting
            // a JSON-object opener. When R1 fires, the prefix IS the
            // structural commitment.
            //
            // R2 (cmd_prefix) DOES compose: the grammar walks from
            // token 0 normally and the prefix appears as a `pattern`
            // on the cmd field, enforced by the existing string-body
            // walker. So we only suppress envelope install for R1.
            if let Some(envelope) =
                tool_envelope_schema_for_with_env_and_cmd_prefix(request, request.cmd_prefix.as_deref())
            {
                req.structured_output = Some(envelope);
            }
        }
        // Per-request `enable_thinking` toggle. The OpenAI extension
        // `chat_template_kwargs: { enable_thinking: <bool> }` is what
        // vLLM and llama-server both expose; we honour the same shape
        // so callers (RemoteApiProvider in particular) don't need a
        // Sovereign-specific extension. None on the wire means "fall
        // through to the embedded provider's default" — which is
        // `enable_thinking: false` in `apply_chat_template_oaicompat`.
        req.enable_thinking = extract_enable_thinking(request.chat_template_kwargs.as_ref());
        let (speed, slot_picker) = if req.tools.is_some() {
            (sovereign_core::types::Speed::Slow, "tools_bias_slow")
        } else {
            (
                crate::oicp_select::pick_slot_for_oicp(self.provider.as_ref(), &req),
                "oicp_select",
            )
        };
        tracing::debug!(
            speed = ?speed,
            slot_picker = %slot_picker,
            has_tools = req.tools.is_some(),
            model_id = req.model_id.as_deref().unwrap_or(""),
            structured_output = req.structured_output.is_some(),
            "inference_adapter:slot_selected"
        );
        req.with_speed(speed)
    }
}

/// Parse a model response that was generated under a tool-envelope
/// grammar. The whole response is expected to be ONE balanced JSON
/// object of shape `{"name": <tool>, "arguments": <params>}`. Used
/// only on the grammar-constrained path — the free-form path still
/// goes through `parse_tool_calls_with_errors` which expects
/// `<tool_call>...</tool_call>` markup.
///
/// Returns an empty vec when the response doesn't parse as a tool
/// envelope (caller falls back to the marker-based parser).
pub(crate) fn parse_tool_envelope_direct(
    text: &str,
) -> Vec<sovereign_inference::embedded::ParsedToolCall> {
    let trimmed = text.trim();
    let parsed: Result<serde_json::Value, _> =
        serde_json::from_str(trimmed).or_else(|_| {
            // Mirror the parser-hardening pre-pass: unescaped raw
            // newlines inside string values are common from
            // Qwen-Coder. Re-try on a normalized copy.
            let fixed = sovereign_inference::embedded::escape_unescaped_control_chars_in_string_values(trimmed);
            serde_json::from_str(&fixed)
        });
    let Ok(obj) = parsed else { return Vec::new() };
    let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    let args_str = match obj.get("arguments") {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
        Some(v) => v.to_string(),
        None => "{}".to_string(),
    };
    vec![sovereign_inference::embedded::ParsedToolCall {
        name: name.to_string(),
        arguments: args_str,
    }]
}

/// True when the `SOVEREIGN_FORCE_TOOL_CALLS` env-var is set to a
/// truthy value (`1` / `true`, case-insensitive). Tools-using
/// clients that don't pass `tool_choice` (opencode, Aider, the
/// Anthropic SDK's openai-compat shim, …) can opt into the
/// tool-envelope grammar by exporting this on the daemon. Read
/// per-call (not cached) so flipping it at runtime is enough — no
/// daemon restart required for the env var to take effect on the
/// next request.
fn force_tool_calls_env() -> bool {
    std::env::var("SOVEREIGN_FORCE_TOOL_CALLS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// `tool_envelope_schema_for` plus the `SOVEREIGN_FORCE_TOOL_CALLS`
/// env-var override. When the env var is set and `tools` is
/// non-empty but `tool_choice` was omitted, we synthesize a
/// `"required"` tool_choice for the schema-builder. The original
/// request is left untouched — only the schema decision sees the
/// override. Used in both:
///   - `build_completion_request` (decide whether to install the
///     grammar in `structured_output`),
///   - `chat_completion` parse-mode dispatch (decide between the
///     direct-envelope parser and the marker-based parser).
pub(crate) fn tool_envelope_schema_for_with_env(
    request: &ChatCompletionRequest,
) -> Option<serde_json::Value> {
    tool_envelope_schema_for_with_env_and_cmd_prefix(request, None)
}

/// Same as `tool_envelope_schema_for_with_env`, but also decorates the
/// `cmd` parameter of `exec_command` tools with a `pattern` that pins
/// the literal prefix. `JsonConstraint` consumes `pattern` (prefix
/// subset only — see `compile_schema`) and masks any byte that
/// wouldn't extend the prefix until the prefix is fully emitted.
/// Family-agnostic; called from `build_completion_request` when
/// `request.cmd_prefix.is_some()`.
pub(crate) fn tool_envelope_schema_for_with_env_and_cmd_prefix(
    request: &ChatCompletionRequest,
    cmd_prefix: Option<&str>,
) -> Option<serde_json::Value> {
    let mut envelope = tool_envelope_schema_for(request).or_else(|| {
        // Replicate the env-var fallback below for the prefix-using path.
        if force_tool_calls_env() {
            let tc = request.tool_choice.as_ref().map(|v| v.as_str()).flatten();
            if tc == Some("none") {
                return None;
            }
            let mut overridden = request.clone();
            overridden.tool_choice = Some(serde_json::json!("required"));
            tool_envelope_schema_for(&overridden)
        } else {
            None
        }
    })?;
    if let Some(prefix) = cmd_prefix.filter(|s| !s.is_empty()) {
        inject_cmd_pattern(&mut envelope, prefix);
    }
    Some(envelope)
}

/// Walk an envelope schema, find any `exec_command` variant in `oneOf`,
/// and inject a `pattern` on its `arguments.cmd` string field. The
/// pattern is the literal prefix anchored at start (`^literal...`).
/// `JsonConstraint`'s string-body walker recognises this subset and
/// enforces it as a forced-prefix on the cmd field.
fn inject_cmd_pattern(schema: &mut serde_json::Value, prefix: &str) {
    let Some(variants) = schema.get_mut("oneOf").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let pattern = format!("^{}", regex_escape_literal(prefix));
    for variant in variants {
        // Variants are `{type:"object", properties:{name:{enum:["X"]}, arguments:{...}}}`.
        let name_const = variant
            .get("properties")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.get("enum"))
            .and_then(|e| e.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if name_const.as_deref() != Some("exec_command") {
            continue;
        }
        let Some(args) = variant
            .get_mut("properties")
            .and_then(|p| p.get_mut("arguments"))
        else {
            continue;
        };
        let Some(cmd) = args.get_mut("properties").and_then(|p| p.get_mut("cmd")) else {
            continue;
        };
        let Some(cmd_obj) = cmd.as_object_mut() else {
            continue;
        };
        cmd_obj.insert(
            "pattern".to_string(),
            serde_json::Value::String(pattern.clone()),
        );
    }
}

/// Escape regex metacharacters in `s` so it matches as a literal.
/// JsonConstraint's pattern parser only accepts the literal-prefix
/// subset (see `compile_schema`), so this escapes both standard
/// metacharacters and the chars the parser would refuse to treat as
/// literal. Idempotent.
fn regex_escape_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for ch in s.chars() {
        if matches!(
            ch,
            '\\' | '^' | '$' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// When the request has `tool_choice = "required"` (OpenAI semantics
/// "model MUST call a tool") and a non-empty `tools` array, build a
/// JSON Schema describing the legal tool-envelope shape:
///
/// ```json
/// {
///   "oneOf": [
///     { "type": "object",
///       "properties": {
///         "name": { "const": "<tool_1_name>" },
///         "arguments": <tool_1.parameters schema>
///       },
///       "required": ["name", "arguments"],
///       "additionalProperties": false
///     },
///     ... one entry per tool ...
///   ]
/// }
/// ```
///
/// The sampler installs this as a logit mask via
/// `sovereign_inference::json_constraint::JsonConstraint`. Tokens
/// that would lead to a body that violates the schema (unbalanced
/// braces, raw control chars in strings, an unknown `name` value,
/// the wrong type for an `arguments.<field>`, etc.) are masked out
/// at decode time — which closes the three observed failure modes
/// (FINAL-Bench drop-closing-tag, Qwen-Coder raw-`\n`,
/// character-drop-in-Rust) at the structural layer rather than via
/// the lenient parser.
///
/// Returns `None` when:
///   - `tools` is absent or empty,
///   - `tool_choice` is not the literal string `"required"` (we
///     intentionally don't engage on `"auto"` because the model
///     legitimately needs to be able to emit text-only turns to
///     end an opencode session),
///   - any tool's `parameters` is missing or not a JSON object
///     (the sampler can't enforce a schema we can't construct).
///
/// Note on the wrapper: the daemon's chat template emits
/// `<tool_call>` markers around the model's JSON output. The
/// constraint operates on the raw token stream, so the surrounding
/// markers are untouched — the model writes them as part of the
/// templated prefix/suffix, the constraint guards only the JSON in
/// between.
pub(crate) fn tool_envelope_schema_for(
    request: &ChatCompletionRequest,
) -> Option<serde_json::Value> {
    let tools = request.tools.as_ref()?;
    if tools.is_empty() {
        return None;
    }
    // tool_choice is held as opaque `serde_json::Value` so forward-
    // compat shapes pass through. We engage only on the literal
    // `"required"` string variant — the structured form
    // `{"type":"function","function":{"name":"x"}}` is a future
    // refinement (would build a single-tool schema, no oneOf).
    let tc = request.tool_choice.as_ref()?;
    if tc.as_str() != Some("required") {
        return None;
    }
    let mut variants: Vec<serde_json::Value> = Vec::with_capacity(tools.len());
    for t in tools {
        if !t.function.parameters.is_object() {
            // Skip tools whose params we can't constrain. Better to
            // disengage entirely than to install a permissive grammar
            // that masks one tool but lets others through unguarded.
            return None;
        }
        let mut props = serde_json::Map::new();
        // `name` is a string with a single allowed value. The
        // JsonConstraint compiler requires `type` alongside `const`
        // (otherwise it can't pick a primitive validator); use
        // `enum: ["x"]` since that maps cleanly onto its
        // string-with-enum validator.
        props.insert(
            "name".to_string(),
            serde_json::json!({ "type": "string", "enum": [t.function.name] }),
        );
        props.insert(
            "arguments".to_string(),
            t.function.parameters.clone(),
        );
        variants.push(serde_json::json!({
            "type": "object",
            "properties": props,
            "required": ["name", "arguments"],
            "additionalProperties": false,
        }));
    }
    Some(serde_json::json!({ "oneOf": variants }))
}

/// Resolve the effective `think_budget` for a request.
///
/// - Caller-supplied value wins (any explicit `Some(n)`).
/// - Otherwise, when tools are present and non-empty, default to
///   `Some(0)`. Tool-using clients (opencode, Aider, MCP-driven
///   agents) don't pass the Commonwealth extension and almost
///   always want the model to spend its output budget on the tool
///   call, not a `<think>` block. The dominant failure mode without
///   this default: FINAL-Bench-35B / Qwen3.5 burns ~14.5K tokens
///   reasoning, then runs out of budget mid-`<tool_call>{...`,
///   yielding a parse error and a stop-finished response with no
///   tool calls.
/// - Otherwise (no tools, no explicit budget), return `None` and
///   let the embedded provider's chat-template default decide.
pub(crate) fn resolve_think_budget(
    request: &ChatCompletionRequest,
) -> Option<usize> {
    if let Some(n) = request.think_budget {
        return Some(n as usize);
    }
    if request.tools.as_ref().is_some_and(|t| !t.is_empty()) {
        return Some(0);
    }
    None
}

/// Pull `enable_thinking: bool` out of the OpenAI extension
/// `chat_template_kwargs` blob. Returns `None` when the kwargs
/// object is missing or doesn't carry the key — leaves the embedded
/// provider's default (currently `false`) in charge. Other keys in
/// the blob are accepted but ignored.
pub(crate) fn extract_enable_thinking(
    kwargs: Option<&serde_json::Value>,
) -> Option<bool> {
    kwargs
        .and_then(|v| v.get("enable_thinking"))
        .and_then(|v| v.as_bool())
}

/// Pull a JSON Schema out of the OpenAI `response_format` envelope.
/// Returns `None` for unrecognised or missing shapes so we don't
/// propagate junk into the sampler.
pub(crate) fn extract_response_format_schema(
    rf: &serde_json::Value,
) -> Option<serde_json::Value> {
    let kind = rf.get("type").and_then(|v| v.as_str())?;
    match kind {
        "json_schema" => rf
            .get("json_schema")
            .and_then(|js| js.get("schema"))
            .cloned(),
        "json_object" => Some(serde_json::json!({"type": "object"})),
        _ => None,
    }
}

/// Remove every `<tool_call>...</tool_call>` block from the response
/// text. Used when the adapter has already extracted tool calls into
/// the structured `tool_calls` field — keeping the raw markup in
/// `content` causes clients to double-render.
pub(crate) fn strip_tool_call_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while cursor < text.len() {
        if let Some(rel) = text[cursor..].find("<tool_call>") {
            let start = cursor + rel;
            out.push_str(&text[cursor..start]);
            match text[start..].find("</tool_call>") {
                Some(end_rel) => {
                    cursor = start + end_rel + "</tool_call>".len();
                }
                None => {
                    // Unterminated — preserve the rest as-is so the
                    // model's unfinished output remains visible.
                    out.push_str(&text[start..]);
                    return out;
                }
            }
        } else {
            out.push_str(&text[cursor..]);
            break;
        }
    }
    // Tidy up double-newlines left behind by stripped blocks.
    out.trim().to_string()
}

/// Guard rejecting tool-enabled requests that would land on the Fast
/// slot. The Fast slot (Qwen3-1.7B in the current stack) has no
/// tools-aware chat template, so executing a tool request against it
/// would silently drop every tool the caller listed. Per M2 scoping
/// we fail loudly with a structured error instead of silently
/// escalating — masking the latency/capacity implication hides
/// capacity issues from operators.
///
/// Returns `Err(message)` only when **both** conditions hold:
/// - `request.tools.is_some()` AND non-empty;
/// - the slot picker landed on `Speed::Fast`.
///
/// Pure function; no I/O. Unit-tested without spinning up the mesh.
pub(crate) fn guard_tools_on_fast(
    tools_present: bool,
    picked_speed: sovereign_core::types::Speed,
) -> Result<(), String> {
    if tools_present && picked_speed == sovereign_core::types::Speed::Fast {
        return Err(
            "fast slot does not support tool_calls; re-send with preferred_speed=Slow \
             (see sovereign atos probe-driver for a capability probe)"
                .to_string(),
        );
    }
    Ok(())
}

#[async_trait]
impl LocalInferenceService for SovereignInferenceAdapter {
    async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, String> {
        tracing::debug!(
            message_count = request.messages.len(),
            model = request.model.as_deref().unwrap_or(""),
            has_tools = request.tools.as_ref().is_some_and(|t| !t.is_empty()),
            stream = false,
            "inference_adapter:chat_completion_entry"
        );
        // Glassbox prompt-size accounting + tool-profile trim +
        // opt-in compaction BEFORE anything reads the request.
        //
        // Order: pre_compact log → tool-profile trim (drops tool
        // entries by name from request.tools[]) → opt-in compactor
        // (caps oversized tool-result bodies) → post-trim log.
        // The tool-profile pass runs on every request; the registry
        // is empty/Wildcard by default (zero work when unconfigured),
        // and when a profile is active its filter() drops tools by
        // name and logs the count.
        let pre_report = crate::prompt_compactor::PromptSizeReport::measure(&request);
        pre_report.log("pre_compact");
        let _profile = crate::tool_profile::apply(
            crate::tool_profile::global(),
            &mut request,
        );
        let compactor = crate::prompt_compactor::PromptCompactor::from_env();
        if compactor.is_active() {
            compactor.compact(&mut request);
        }
        // Post-compact log fires when ANY transformation changed the
        // request. The total-char check covers both profile drops
        // and tool-result truncation; the explicit `is_active`
        // covers the rare case where the compactor was active but
        // produced no change (no oversized tool messages today).
        let post_report = crate::prompt_compactor::PromptSizeReport::measure(&request);
        if post_report.total_chars() != pre_report.total_chars() || compactor.is_active() {
            post_report.log("post_compact");
        }

        let req = self.build_completion_request(&request);

        // Slot-policy guard: fail loud when a tool-enabled request
        // lands on the Fast slot. Emit a structured warn log so
        // operators see the attempt even if the client buries the
        // error. Intentionally checked BEFORE inference kicks off —
        // a rejection costs a few microseconds, not a full inference
        // call.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        if let Err(msg) = guard_tools_on_fast(tools_present, req.preferred_speed) {
            tracing::warn!(
                tools_count = request.tools.as_ref().map(|t| t.len()).unwrap_or(0),
                preferred_speed = ?req.preferred_speed,
                "inference adapter: rejecting tool request on fast slot"
            );
            return Err(msg);
        }

        let started = std::time::Instant::now();
        let resp = self
            .provider
            .complete(&req)
            .await
            .map_err(|e| format!("{e}"))?;

        // Tool-call extraction. Only parse when the caller supplied
        // tools; otherwise any stray `<tool_call>` text the model
        // produced stays in `content` untouched. `with_errors` variant
        // so parse failures are visible in telemetry instead of
        // silently collapsing to zero calls.
        //
        // Two parse modes:
        //   • grammar_constrained — the daemon installed the
        //     tool-envelope JSON-schema sampler (see
        //     `tool_envelope_schema_for`). The model's output is one
        //     balanced JSON object matching the schema, with no
        //     `<tool_call>` wrapper. We try the direct-envelope path
        //     first; if that fails for any reason, we fall through to
        //     the marker-based parser so a degraded model emission
        //     still has a chance.
        //   • free-form — the legacy `<tool_call>{...}</tool_call>`
        //     markup the chat template emits when no grammar is set.
        let grammar_constrained = req.structured_output.is_some()
            && tool_envelope_schema_for_with_env(&request).is_some();
        if tools_present {
            tracing::debug!(
                grammar_constrained,
                "inference_adapter:tool_parse_mode"
            );
        }
        // When the request set an `assistant_prefix`, the inference
        // layer appended it to the rendered prompt — but it lives in
        // the prompt's KV cache, not in `resp.text` (which is just
        // the model's *generated* continuation). Stitch the prefix
        // back on for tool-call parsing so the `<tool_call>{...}`
        // opener encoded into the prefix lines up with the
        // `</tool_call>` closer in the generated tail.
        let text_for_parsing: String = match req.assistant_prefix.as_deref() {
            Some(p) if !p.is_empty() => {
                let mut joined = String::with_capacity(p.len() + resp.text.len());
                joined.push_str(p);
                joined.push_str(&resp.text);
                joined
            }
            _ => resp.text.clone(),
        };
        let (parsed_calls, parse_errors) = if tools_present {
            if grammar_constrained {
                let direct = parse_tool_envelope_direct(&text_for_parsing);
                if !direct.is_empty() {
                    tracing::debug!(
                        tool_call_count = direct.len(),
                        "inference_adapter:tool_parse_grammar_direct_ok"
                    );
                    (direct, Vec::new())
                } else {
                    tracing::debug!(
                        "inference_adapter:tool_parse_grammar_direct_empty_falling_back_to_marker"
                    );
                    sovereign_inference::embedded::parse_tool_calls_with_errors(&text_for_parsing)
                }
            } else {
                sovereign_inference::embedded::parse_tool_calls_with_errors(&text_for_parsing)
            }
        } else {
            (Vec::new(), Vec::new())
        };
        for raw in &parse_errors {
            tracing::warn!(
                payload = %raw,
                "inference adapter: tool_call parse failed"
            );
        }

        // Source-content validation: walk each tool's parameter
        // schema for `x-source-content` markers, look up the value
        // in arguments, run any registered validators. Today the
        // registry is empty (no language validators wired yet) so
        // the call is a fast no-op; the wiring point is the win
        // — concrete validators slot in here without changing
        // chat_completion. See `source_content_validator` module.
        if let Some(tools) = request.tools.as_ref() {
            let registry = crate::source_content_validator::ValidatorRegistry::new();
            let _findings = crate::source_content_validator::validate_tool_calls(
                &parsed_calls,
                tools,
                &registry,
            );
            // Findings already logged inside validate_tool_calls.
            // Discard the returned Vec — when concrete validators
            // ship, callers may surface findings to the response;
            // for now observability via tracing is the contract.
        }

        let tool_calls_out: Option<Vec<ToolCall>> = if parsed_calls.is_empty() {
            None
        } else {
            Some(
                parsed_calls
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| ToolCall {
                        id: format!(
                            "call_{}_{}",
                            started.elapsed().as_micros(),
                            i
                        ),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: c.name,
                            arguments: c.arguments,
                        },
                    })
                    .collect(),
            )
        };

        // When the model issued tool calls, strip the raw tool-call
        // markup out of the returned content so downstream clients
        // don't re-render it. The structured `tool_calls` field is
        // the authoritative signal.
        let clean_content = if tool_calls_out.is_some() {
            strip_tool_call_blocks(&resp.text)
        } else {
            resp.text
        };

        tracing::info!(
            latency_ms = started.elapsed().as_millis() as u64,
            model = %resp.model_id,
            tokens = resp.tokens_used,
            tool_calls = tool_calls_out.as_ref().map(|c| c.len()).unwrap_or(0),
            parse_errors = parse_errors.len(),
            "sovereign inference adapter: complete served"
        );

        let finish_reason = if tool_calls_out.is_some() {
            "tool_calls".to_string()
        } else {
            "stop".to_string()
        };
        let assistant_msg = ChatMessage {
            role: "assistant".into(),
            content: clean_content,
            tool_call_id: None,
            tool_calls: tool_calls_out,
        };
        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-{}", started.elapsed().as_micros()),
            object: "chat.completion".into(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            model: resp.model_id,
            choices: vec![ChatChoice {
                index: 0,
                message: assistant_msg,
                finish_reason: Some(finish_reason),
            }],
            usage: Some(Usage {
                prompt_tokens: resp.prompt_tokens as u32,
                completion_tokens: resp
                    .tokens_used
                    .saturating_sub(resp.prompt_tokens) as u32,
                total_tokens: resp.tokens_used as u32,
            }),
        })
    }

    async fn chat_completion_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = commonwealth_api::openai_types::StreamFrame> + Send>>,
        String,
    > {
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        tracing::debug!(
            message_count = request.messages.len(),
            model = request.model.as_deref().unwrap_or(""),
            has_tools = tools_present,
            stream = true,
            "inference_adapter:chat_completion_stream_entry"
        );

        // Streaming + tool_calls path. Local backends parse the
        // `<tool_call>` markup AFTER the model finishes generating, so
        // genuine token-by-token streaming for tool turns isn't
        // possible without re-architecting the parser. Instead we
        // route through the non-streaming `chat_completion` (which
        // already handles tool extraction, content cleanup, and
        // finish_reason logic) and synthesize an OpenAI-shaped stream
        // from the response. opencode and other clients that always
        // request `stream=true` once tools are bound now succeed
        // instead of getting a 503.
        if tools_present {
            // Tools path delegates to chat_completion; the compactor
            // runs there. Don't compact twice.
            let resp = self.chat_completion(request).await?;
            let frames = synthesize_tool_stream(resp);
            tracing::info!(
                "sovereign inference adapter: tools-streaming served via synthetic SSE \
                 ({} synthetic frame(s))",
                frames.len()
            );
            return Ok(Box::pin(futures::stream::iter(frames)));
        }

        // Non-tools streaming: run the compactor + tool-profile pass
        // here so the same glassbox/throughput surface applies
        // regardless of tools. The tool-profile pass is a no-op
        // when `request.tools` is None (the streaming-no-tools path
        // by construction), but we still apply for consistency and
        // future-proofing — a request with empty tools[] but a
        // profile header would still get the empty allow-list path
        // treated correctly.
        let pre_report = crate::prompt_compactor::PromptSizeReport::measure(&request);
        pre_report.log("pre_compact");
        let _profile = crate::tool_profile::apply(
            crate::tool_profile::global(),
            &mut request,
        );
        let compactor = crate::prompt_compactor::PromptCompactor::from_env();
        if compactor.is_active() {
            compactor.compact(&mut request);
        }
        let post_report = crate::prompt_compactor::PromptSizeReport::measure(&request);
        if post_report.total_chars() != pre_report.total_chars() || compactor.is_active() {
            post_report.log("post_compact");
        }

        let req = self.build_completion_request(&request);
        let inner = self
            .provider
            .complete_stream_with_finish(&req)
            .await
            .map_err(|e| format!("{e}"))?;
        tracing::info!(
            "sovereign inference adapter: typed streaming started"
        );
        // Translate sovereign_core::types::StreamFrame →
        // commonwealth_api::openai_types::StreamFrame. The two
        // shapes are identical by design (see openai_types.rs);
        // translation is a per-variant copy.
        let mapped = inner.map(translate_stream_frame);
        Ok(Box::pin(mapped))
    }

    fn provider_manifest(&self) -> Option<ProviderManifest> {
        Some(crate::oicp_synthesis::build_self_manifest(self.provider.as_ref()))
    }

    async fn embed(&self, input: &str) -> Result<Vec<f32>, String> {
        // Delegate to the underlying provider's EmbedSlot. The
        // commonwealth-api handler wraps the returned vector in an
        // OpenAI-shape `EmbeddingResponse`.
        self.provider
            .embed(input)
            .await
            .map_err(|e| format!("{e}"))
    }

    // ── Runtime slot management ─────────────────────────────────
    //
    // These delegate to the InferenceProvider trait, which has
    // default `Err(...)` implementations for non-embedded providers
    // (remote API, mesh peer). Only `EmbeddedLlamaCpp` overrides
    // them. The HTTP handler returns 501/400 when the underlying
    // provider can't service the request.

    async fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String, String> {
        self.provider
            .load_extra_slot(slot_name, path, context_size)
            .map_err(|e| format!("{e}"))
    }

    async fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>, String> {
        self.provider
            .unload_extra_slot(slot_name)
            .map_err(|e| format!("{e}"))
    }

    async fn extras_inventory(&self) -> Vec<(String, String)> {
        self.provider.extras_inventory()
    }

    async fn warmup_primary(&self) -> Result<(), String> {
        self.provider
            .warmup_primary()
            .await
            .map_err(|e| format!("{e}"))
    }
}

#[cfg(test)]
mod guard_tests {
    use super::guard_tools_on_fast;
    use sovereign_core::types::Speed;

    #[test]
    fn tool_request_on_slow_slot_passes() {
        assert!(guard_tools_on_fast(true, Speed::Slow).is_ok());
        assert!(guard_tools_on_fast(true, Speed::Medium).is_ok());
    }

    #[test]
    fn tool_request_on_fast_slot_rejected() {
        let err = guard_tools_on_fast(true, Speed::Fast).unwrap_err();
        assert!(err.contains("fast slot"));
        assert!(err.contains("tool_calls"));
        assert!(err.contains("preferred_speed=Slow"));
    }

    #[test]
    fn toolless_request_on_fast_slot_passes() {
        assert!(guard_tools_on_fast(false, Speed::Fast).is_ok());
    }

    #[test]
    fn toolless_request_on_slow_slot_passes() {
        assert!(guard_tools_on_fast(false, Speed::Slow).is_ok());
    }
}

#[cfg(test)]
mod adapter_translation_tests {
    use super::{strip_tool_call_blocks, SovereignInferenceAdapter};
    use commonwealth_api::openai_types::{
        ChatCompletionRequest, ChatMessage, FunctionCall, ToolCall, ToolDefinition, ToolFunction,
    };

    fn tool_def(name: &str) -> ToolDefinition {
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: Some(format!("description of {name}")),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"x": {"type": "string"}},
                    "required": ["x"],
                }),
            },
        }
    }

    fn user(content: &str) -> ChatMessage {
        ChatMessage::new("user", content)
    }

    #[test]
    fn flatten_preserves_tool_message_id() {
        let req = ChatCompletionRequest {
            model: None,
            messages: vec![
                user("what's the weather?"),
                ChatMessage {
                    role: "assistant".into(),
                    content: String::new(),
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_123".into(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: "get_weather".into(),
                            arguments: r#"{"city":"SF"}"#.into(),
                        },
                    }]),
                },
                ChatMessage {
                    role: "tool".into(),
                    content: r#"{"temperature":62}"#.into(),
                    tool_call_id: Some("call_123".into()),
                    tool_calls: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: Some(vec![tool_def("get_weather")]),
            tool_choice: Some(serde_json::json!("auto")),
            oicp: None,
                    response_format: None,
                    chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        };
        let (prompt, _system) = SovereignInferenceAdapter::flatten(&req);
        // The prior tool call is replayed as a <tool_call> block so
        // Qwen3.5's template sees the model's own previous turn in a
        // shape it recognizes.
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("get_weather"));
        // The tool result carries its call id so the model can
        // correlate it with the originating call.
        assert!(prompt.contains("Tool[call_123]:"));
        assert!(prompt.contains(r#"{"temperature":62}"#));
    }

    #[test]
    fn forward_tools_translates_schema() {
        let req = ChatCompletionRequest {
            model: None,
            messages: vec![user("hi")],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: Some(vec![tool_def("a"), tool_def("b")]),
            tool_choice: None,
            oicp: None,
                    response_format: None,
                    chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        };
        let forwarded = SovereignInferenceAdapter::forward_tools(&req).unwrap();
        assert_eq!(forwarded.len(), 2);
        assert_eq!(forwarded[0].name, "a");
        assert_eq!(forwarded[1].name, "b");
        assert_eq!(forwarded[0].description.as_deref(), Some("description of a"));
    }

    #[test]
    fn forward_tools_empty_returns_none() {
        let req = ChatCompletionRequest {
            model: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: Some(Vec::new()),
            tool_choice: None,
            oicp: None,
                    response_format: None,
                    chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        };
        assert!(SovereignInferenceAdapter::forward_tools(&req).is_none());
    }

    #[test]
    fn strip_tool_call_blocks_removes_markup() {
        let text = "Let me check.\n<tool_call>{\"name\":\"f\",\"arguments\":{}}</tool_call>\nDone.";
        let stripped = strip_tool_call_blocks(text);
        assert!(!stripped.contains("<tool_call>"));
        assert!(!stripped.contains("</tool_call>"));
        // Surrounding prose is preserved.
        assert!(stripped.contains("Let me check."));
        assert!(stripped.contains("Done."));
    }

    #[test]
    fn strip_tool_call_blocks_handles_multiple() {
        let text = "<tool_call>a</tool_call>mid<tool_call>b</tool_call>end";
        let stripped = strip_tool_call_blocks(text);
        assert_eq!(stripped, "midend");
    }

    #[test]
    fn strip_tool_call_blocks_tolerates_unterminated() {
        // Defensive: a truncated model output shouldn't panic — we
        // preserve the tail so operators can see what happened.
        let text = "ok <tool_call>{\"name\":\"never_closed\"";
        let stripped = strip_tool_call_blocks(text);
        assert!(stripped.contains("never_closed"));
    }

    fn req_with(
        tools: Option<Vec<ToolDefinition>>,
        think_budget: Option<u32>,
    ) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages: vec![user("hi")],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools,
            tool_choice: None,
            oicp: None,
            response_format: None,
            chat_template_kwargs: None,
            think_budget,
            tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        }
    }

    #[test]
    fn think_budget_defaults_to_zero_when_tools_present() {
        // Witnesses the FINAL-Bench-35B failure mode: 14.5K think
        // tokens, then a truncated `<tool_call>`. opencode/Aider/MCP
        // clients don't know about think_budget, so the daemon
        // defaults to Some(0) when tools are present so the model
        // emits the tool call without a chain-of-thought prelude.
        let req = req_with(Some(vec![tool_def("write")]), None);
        assert_eq!(super::resolve_think_budget(&req), Some(0));
    }

    #[test]
    fn think_budget_respects_explicit_caller_value_with_tools() {
        // If a caller did want thinking with tools (e.g. a debugging
        // session), don't override their explicit choice.
        let req = req_with(Some(vec![tool_def("write")]), Some(2048));
        assert_eq!(super::resolve_think_budget(&req), Some(2048));
    }

    #[test]
    fn think_budget_is_none_without_tools() {
        // No tools and no explicit budget → fall through to the
        // embedded provider's chat-template default.
        let req = req_with(None, None);
        assert_eq!(super::resolve_think_budget(&req), None);
    }

    #[test]
    fn think_budget_is_none_when_tools_array_is_empty() {
        // Empty tools array is semantically "no tools" — same as None.
        let req = req_with(Some(vec![]), None);
        assert_eq!(super::resolve_think_budget(&req), None);
    }

    fn req_with_tool_choice(
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<serde_json::Value>,
    ) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages: vec![user("hi")],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools,
            tool_choice,
            oicp: None,
            response_format: None,
            chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        }
    }

    #[test]
    fn tool_envelope_schema_engages_on_required_with_tools() {
        let req = req_with_tool_choice(
            Some(vec![tool_def("write")]),
            Some(serde_json::json!("required")),
        );
        let schema = super::tool_envelope_schema_for(&req)
            .expect("schema should be built");
        // oneOf with one variant for the single tool.
        let variants = schema.get("oneOf").and_then(|v| v.as_array()).unwrap();
        assert_eq!(variants.len(), 1);
        // The variant binds `name` to the tool's literal name via
        // `enum: ["..."]` (the JsonConstraint compiler can't validate
        // bare `const` without `type`, so we use enum-of-one instead).
        let name_enum = variants[0]
            .pointer("/properties/name/enum")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(name_enum.len(), 1);
        assert_eq!(name_enum[0].as_str(), Some("write"));
        // additionalProperties is false so the model can't emit
        // unrecognised top-level keys alongside `name`+`arguments`.
        assert_eq!(
            variants[0].pointer("/additionalProperties"),
            Some(&serde_json::json!(false))
        );
    }

    #[test]
    fn tool_envelope_schema_skipped_on_auto_choice() {
        // tool_choice="auto" must not engage — the model needs to be
        // free to emit text-only turns to end an opencode session.
        let req = req_with_tool_choice(
            Some(vec![tool_def("write")]),
            Some(serde_json::json!("auto")),
        );
        assert!(super::tool_envelope_schema_for(&req).is_none());
    }

    #[test]
    fn tool_envelope_schema_skipped_when_no_tool_choice_set() {
        // Caller didn't set tool_choice. Default behaviour is "auto"
        // semantically, and we must not constrain.
        let req = req_with_tool_choice(Some(vec![tool_def("write")]), None);
        assert!(super::tool_envelope_schema_for(&req).is_none());
    }

    #[test]
    fn tool_envelope_schema_skipped_when_tools_empty_under_required() {
        // `required` with zero tools is malformed; refuse to constrain
        // (empty oneOf would mask everything → model can never finish).
        let req = req_with_tool_choice(
            Some(vec![]),
            Some(serde_json::json!("required")),
        );
        assert!(super::tool_envelope_schema_for(&req).is_none());
    }

    /// Per-test-module lock for tests that mutate
    /// `SOVEREIGN_FORCE_TOOL_CALLS`. Three callers, all in this file.
    /// The promise "tests run fast so the race won't matter" turned
    /// out to be a flake under repo-wide parallel `cargo test` — pin
    /// it with an actual mutex.
    fn force_tool_calls_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
            std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn force_tool_calls_env_engages_schema_when_choice_omitted() {
        let _guard = force_tool_calls_env_lock();
        std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
        let req = req_with_tool_choice(Some(vec![tool_def("write")]), None);
        let schema = super::tool_envelope_schema_for_with_env(&req);
        std::env::remove_var("SOVEREIGN_FORCE_TOOL_CALLS");
        assert!(schema.is_some(), "env override should synthesize tool_choice=required");
    }

    #[test]
    fn force_tool_calls_env_overrides_auto() {
        let _guard = force_tool_calls_env_lock();
        // Operator opted in via env: even an explicit "auto" gets
        // upgraded to "required" so the grammar engages. The opt-in
        // is global to the daemon; clients that need text-only turns
        // shouldn't run against a daemon with this var set.
        std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
        let req = req_with_tool_choice(
            Some(vec![tool_def("write")]),
            Some(serde_json::json!("auto")),
        );
        let schema = super::tool_envelope_schema_for_with_env(&req);
        std::env::remove_var("SOVEREIGN_FORCE_TOOL_CALLS");
        assert!(schema.is_some(), "env override should upgrade auto to required");
    }

    #[test]
    fn force_tool_calls_env_respects_explicit_none() {
        let _guard = force_tool_calls_env_lock();
        // tool_choice="none" semantically means "model must NOT call a
        // tool". Even with the env var set, refuse to override that.
        std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
        let req = req_with_tool_choice(
            Some(vec![tool_def("write")]),
            Some(serde_json::json!("none")),
        );
        let schema = super::tool_envelope_schema_for_with_env(&req);
        std::env::remove_var("SOVEREIGN_FORCE_TOOL_CALLS");
        assert!(schema.is_none(), "explicit none must NOT be overridden");
    }

    #[test]
    fn parse_tool_envelope_direct_extracts_clean_json() {
        // Grammar-constrained output is a single balanced JSON object
        // with no `<tool_call>` wrapper.
        let text = r#"{"name":"write","arguments":{"filePath":"a.rs","content":"fn main(){}"}}"#;
        let calls = super::parse_tool_envelope_direct(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("filePath"));
    }

    #[test]
    fn parse_tool_envelope_direct_returns_empty_on_non_envelope() {
        // Random text or malformed JSON returns empty so the caller
        // can fall back to the marker-based parser.
        assert!(super::parse_tool_envelope_direct("hello world").is_empty());
        assert!(super::parse_tool_envelope_direct(r#"{"foo":"bar"}"#).is_empty());
        assert!(super::parse_tool_envelope_direct("").is_empty());
    }

    #[test]
    fn parse_tool_envelope_direct_normalizes_raw_newlines() {
        // Same Qwen-Coder failure mode the marker-based parser
        // already handles: raw \n inside content string.
        let text = "{\"name\":\"write\",\"arguments\":{\"path\":\"x\",\"content\":\"line1\nline2\"}}";
        let calls = super::parse_tool_envelope_direct(text);
        assert_eq!(calls.len(), 1, "normalization should recover");
        assert_eq!(calls[0].name, "write");
    }

    #[test]
    fn tool_envelope_schema_oneof_includes_each_tool() {
        let req = req_with_tool_choice(
            Some(vec![tool_def("a"), tool_def("b"), tool_def("c")]),
            Some(serde_json::json!("required")),
        );
        let schema = super::tool_envelope_schema_for(&req).unwrap();
        let variants = schema.get("oneOf").and_then(|v| v.as_array()).unwrap();
        assert_eq!(variants.len(), 3);
        let names: Vec<String> = variants
            .iter()
            .map(|v| {
                v.pointer("/properties/name/enum")
                    .and_then(|x| x.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|x| x.as_str())
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}

