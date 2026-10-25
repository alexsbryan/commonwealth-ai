// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat-completions fields that more than one host spells differently:
//! structured output and thinking. Written here, once, so a mesh hop to a
//! peer daemon and enrich's dispatch to a vendor say the same thing the same
//! way ([`crate::RemoteApiProvider::build_request`] is their one builder).

use serde_json::{json, Value};
use sovereign_contracts::decode_allowlist;
use sovereign_contracts::engine_config::GrammarSupport;
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::structured_output::default_additional_properties_false;
use sovereign_contracts::types::{CompletionRequest, PromptShape};
use std::sync::atomic::Ordering;

use crate::{error_excerpt, RemoteApiProvider};

pub use sovereign_contracts::setup_config::StructuredOutputMode;

/// The `messages` a request goes out as. A conversation goes whole, so the
/// far end's chat template renders its roles, calls and results, as the
/// embedded engine does. On that shape `prompt` is a flattened copy kept for
/// token estimators; sending it put the whole agent history in one user turn
/// labelled `Tool[id]:`, and the model ran on inside its own tool call,
/// writing fake results and turns (e2e-swe arm D, 2026-10-09).
/// Record the body this client sends, for the engine conformance battery
/// (`engine_observe`); a no-op unless a sink is installed.
pub(crate) fn observe_wire_request(body: &Value) {
    sovereign_contracts::engine_observe::observe(|| {
        sovereign_contracts::engine_observe::Observation::WireRequest { body: body.clone() }
    });
}

/// Record the raw body this client received.
pub(crate) fn observe_wire_response(status: u16, body: &str) {
    sovereign_contracts::engine_observe::observe(|| {
        sovereign_contracts::engine_observe::Observation::WireResponse {
            status,
            body: body.to_string(),
        }
    });
}

pub(crate) fn request_messages(request: &CompletionRequest) -> Vec<Value> {
    if let Some(PromptShape::Conversation { messages }) = &request.prompt_shape {
        tracing::debug!(
            messages = messages.len(),
            "oicp_client:request_messages conversation sent whole"
        );
        return messages.clone();
    }
    let mut messages = Vec::with_capacity(2);
    if let Some(system) = &request.system_message {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.push(json!({"role": "user", "content": &request.prompt}));
    messages
}

/// The text `request`'s `assistant_prefix` goes to `host` as: a trailing
/// assistant message, for every host but a Sovereign daemon, which reads the
/// `assistant_prefix` extension field. llama-server copies that field and
/// never reads it (LS server-common.cpp:1386-1391); a trailing assistant
/// message it prefills (`prefill_assistant`, on by default, LS
/// server-common.cpp:1272-1278): the turns before it are rendered, the
/// assistant turn is opened, and the message's content is written after it
/// unclosed, for the model to continue (LS common/chat.cpp:3635-3652, the
/// Qwen3 form at :1203-1217). The one decider for what the request carries
/// and what the answer is stripped of ([`crate::ChatMessage::strip_continued`]).
pub(crate) fn prefix_as_message(request: &CompletionRequest, host: GrammarSupport) -> Option<&str> {
    if host == GrammarSupport::Sovereign {
        return None;
    }
    request
        .assistant_prefix
        .as_deref()
        .filter(|p| !p.is_empty())
}

/// Write `request`'s assistant prefix in `host`'s spelling
/// ([`prefix_as_message`]). After `messages` is on `body`.
pub(crate) fn write_assistant_prefix(
    body: &mut Value,
    request: &CompletionRequest,
    host: GrammarSupport,
) {
    if let Some(prefix) = prefix_as_message(request, host) {
        tracing::debug!(
            target: "oicp_client",
            prefix_chars = prefix.len(),
            "write_assistant_prefix: sent as a trailing assistant message to continue"
        );
        if let Some(messages) = body["messages"].as_array_mut() {
            messages.push(json!({"role": "assistant", "content": prefix}));
        }
    } else if let Some(prefix) = request
        .assistant_prefix
        .as_ref()
        .filter(|_| host == GrammarSupport::Sovereign)
    {
        body["assistant_prefix"] = json!(prefix);
    }
}

/// Write `request`'s decode constraint (a Lark grammar, the URL and
/// evidence-id allow-lists) in `host`'s spelling. Call it after the tools and
/// any `response_format` are on `body`: llama-server takes one `grammar` and
/// refuses it beside tools, so what can ride depends on them.
///
/// A constraint the host cannot enforce is refused, never dropped unseen.
/// The one exception is a Lark grammar beside tools: llama-server's own
/// tool-call grammar takes its place, and the drop is traced.
///
/// Public so the llg-parity corpus (sovereign-inference
/// `llg_parity_corpus_tests`) records the grammar this writes rather than a
/// copy of its rules.
pub fn write_constraints(
    body: &mut Value,
    request: &CompletionRequest,
    host: GrammarSupport,
) -> Result<()> {
    if host == GrammarSupport::Sovereign {
        if let Some(grammar) = &request.lark_grammar {
            body["lark_grammar"] = json!(grammar);
        }
        if let Some(urls) = &request.url_allowlist {
            body["url_allowlist"] = json!(urls);
        }
        if let Some(ids) = &request.evidence_id_allowlist {
            body["evidence_id_allowlist"] = json!(ids);
        }
        return Ok(());
    }
    let no_ids: &[String] = &[];
    let masks = decode_allowlist::llguidance_lark(&[
        (
            &decode_allowlist::URL,
            request.url_allowlist.as_deref().unwrap_or(no_ids),
        ),
        (
            &decode_allowlist::EVIDENCE_ID,
            request.evidence_id_allowlist.as_deref().unwrap_or(no_ids),
        ),
    ]);
    let lark = request.lark_grammar.as_deref();
    if lark.is_none() && masks.is_none() {
        return Ok(());
    }
    let tools_offered = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| !t.is_empty())
        && body.get("tool_choice") != Some(&json!("none"));
    let refuse = |why: &str| {
        tracing::warn!(target: "oicp_client", why, "write_constraints: refused");
        Err(Error::InvalidInput(format!(
            "this host takes one llguidance grammar, and {why}, so the request's \
             constraint cannot be honoured. It is refused rather than answered \
             unconstrained."
        )))
    };
    let grammar = match (lark, masks) {
        (Some(_), Some(_)) => {
            return refuse("a Lark grammar cannot be intersected with an allow-list")
        }
        (None, Some(_)) if tools_offered => {
            return refuse("llama-server refuses a grammar beside tools")
        }
        (Some(_), None) if tools_offered => {
            tracing::debug!(
                target: "oicp_client",
                "write_constraints: lark_grammar dropped beside tools; the host's own \
                 tool-call grammar applies"
            );
            return Ok(());
        }
        (Some(lark), None) if lark.trim_start().starts_with("%llguidance") => lark.to_string(),
        (Some(lark), None) => format!("%llguidance {{}}\n{lark}"),
        (None, Some(masks)) => masks,
        (None, None) => unreachable!("returned above"),
    };
    if body.get("response_format").is_some() {
        return refuse("llama-server lets a grammar override response_format");
    }
    tracing::debug!(
        target: "oicp_client",
        lark = lark.is_some(),
        urls = request.url_allowlist.as_ref().map_or(0, Vec::len),
        evidence_ids = request.evidence_id_allowlist.as_ref().map_or(0, Vec::len),
        "write_constraints: sent as an llguidance grammar"
    );
    body["grammar"] = json!(grammar);
    Ok(())
}

/// Send a named `tool_choice` on `body` as `required` with `tools` narrowed
/// to the named one, where `host` would loosen it. llama-server reads
/// `tool_choice` as a string (LS server-common.cpp:1138): an object fails
/// that read and becomes "auto" (server-common.h:40-52). `required` over a
/// one-tool list is the same constraint in the spelling it reads; the reply
/// is checked for the call (`call_check`). A name the list does not offer is
/// refused (`decode.tool-choice-named`). Call it once `tool_choice` is final.
pub(crate) fn narrow_tool_choice(body: &mut Value, host: GrammarSupport) -> Result<()> {
    let Some(named) = body.get("tool_choice").filter(|tc| tc.is_object()) else {
        return Ok(());
    };
    if host == GrammarSupport::Sovereign {
        return Ok(());
    }
    let named = named.clone();
    let name = named.pointer("/function/name").and_then(Value::as_str);
    let tools = body.get("tools").and_then(Value::as_array);
    let kept: Vec<Value> = tools
        .into_iter()
        .flatten()
        .filter(|t| name.is_some() && t.pointer("/function/name").and_then(Value::as_str) == name)
        .cloned()
        .collect();
    if kept.is_empty() {
        tracing::warn!(target: "oicp_client", tool_choice = %named, "narrow_tool_choice: refused");
        return Err(Error::InvalidInput(format!(
            "tool_choice names {named}, which is not among the tools offered, so it \
             cannot be sent as \"required\" over that one tool. It is refused rather \
             than sent to be loosened to \"auto\"."
        )));
    }
    tracing::debug!(
        target: "oicp_client",
        tool = name.unwrap_or_default(),
        offered = tools.map_or(0, Vec::len),
        "narrow_tool_choice: a named choice sent as \"required\" over that one tool"
    );
    body["tools"] = Value::Array(kept);
    body["tool_choice"] = json!("required");
    Ok(())
}

/// Write `schema` onto `body` in `mode`'s spelling, named after the schema's
/// own `title` (JSON Schema's annotation for exactly that, which a grammar
/// ignores) folded to the alphabet OpenAI accepts for a schema or function
/// name. Untitled, it is `structured`.
///
/// The schema goes out with the closed-object default the embedded engine
/// compiles it with (`structured_output::default_additional_properties_false`):
/// a host follows JSON Schema, where an undeclared property is allowed
/// (`decode.json-schema`).
pub(crate) fn write_structured_output(
    body: &mut Value,
    schema: &Value,
    mode: StructuredOutputMode,
) {
    let name = schema_function_name(schema);
    let mut closed = schema.clone();
    default_additional_properties_false(&mut closed);
    let schema = &closed;
    match mode {
        StructuredOutputMode::JsonSchema => {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {"name": name, "schema": schema, "strict": true},
            });
        }
        StructuredOutputMode::JsonObject => {
            body["response_format"] = json!({"type": "json_object"});
        }
        StructuredOutputMode::ToolUseAuto | StructuredOutputMode::ToolUseForced => {
            let function = json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": "Return the structured result.",
                    "parameters": schema,
                }
            });
            // Beside any tools the caller already offered, never instead of them.
            match body.get_mut("tools").and_then(Value::as_array_mut) {
                Some(tools) => tools.push(function),
                None => body["tools"] = json!([function]),
            }
            body["tool_choice"] = if mode == StructuredOutputMode::ToolUseForced {
                // A forced call and thinking do not ride together: DeepSeek
                // answers 400 and Anthropic refuses the pair. Every spelling
                // off, whatever the request asked for.
                write_thinking(body, Some(0), Some(false));
                json!({"type": "function", "function": {"name": name}})
            } else {
                json!("auto")
            };
        }
    }
}

/// Put every spelling of "think this much" on the body that some
/// OpenAI-compatible host reads. There are four, and a host reads the ones
/// it knows; the rest are unknown fields it drops on the floor:
///
/// - `think_budget`: the Commonwealth daemon's native field
///   (`resolve_think_budget`). `0` makes the daemon inject `/no_think` for
///   SystemPromptToken families (Qwen3 / Qwen3.5 / SmolLM3).
/// - `thinking: {type: disabled|enabled}`: DeepSeek V3.1+ / V4.
/// - `reasoning_budget_tokens`: llama-server's per-request budget for an
///   open think block (LS server-common.cpp:1360-1371, which also reads
///   `thinking_budget_tokens`), written for a positive budget. The block is
///   closed at that many tokens (`decode.think-budget-positive`).
/// - `chat_template_kwargs: {enable_thinking: bool}`: llama-server, vLLM and
///   SGLang hand the map to the Jinja chat template, and the Qwen3 templates
///   read exactly this key. It is the ONLY switch a bare llama-server
///   understands. It is written from `enable_thinking` ([`template_thinking`]
///   says which), and as `false` for a zero budget when that is unset: a
///   zero budget means no thinking on every host. A zero reasoning budget
///   alone still lets the template open a think block (bench/lanes/
///   engine-swap, 2026-10-09: seven routing paraphrases flipped without this).
///
/// The kwarg was missing on enrich's path until 2026-09-08, and the
/// bare-endpoint acceptance (`svrn/crates/corpus-mcp/acceptance.sh`, a
/// Qwen3.6-35B behind a plain llama-server) paid for it on every phase: all
/// 20 wessex-hoard chapters failed Phase 1 as `think_truncated`, phases 3 and
/// 6 returned 0 of 4 and 0 of 164, and the run spent 201,596 completion tokens
/// where the daemon spent 17,940.
pub(crate) fn write_thinking(
    body: &mut Value,
    think_budget: Option<usize>,
    enable_thinking: Option<bool>,
) {
    if let Some(tb) = think_budget {
        body["think_budget"] = json!(tb);
        body["thinking"] = if tb == 0 {
            json!({"type": "disabled"})
        } else {
            body["reasoning_budget_tokens"] = json!(tb);
            json!({"type": "enabled", "budget_tokens": tb})
        };
    }
    let enable = enable_thinking.or((think_budget == Some(0)).then_some(false));
    if let Some(enable) = enable {
        body["chat_template_kwargs"] = json!({ "enable_thinking": enable });
    }
}

/// Ask the host for token logprobs in the OpenAI spelling: `logprobs: true`,
/// and `top_logprobs: k` when alternatives are wanted. Nothing when unasked.
pub(crate) fn write_logprobs(body: &mut Value, request: &CompletionRequest) {
    if let Some(top) = request.logprobs {
        body["logprobs"] = json!(true);
        if top > 0 {
            body["top_logprobs"] = json!(top);
        }
    }
}

/// The repetition and frequency penalties, in llama-server's and OpenAI's
/// spelling, when the request names them. Unnamed, the host's defaults stand.
pub(crate) fn write_penalties(body: &mut Value, request: &CompletionRequest) {
    if let Some(p) = request.repeat_penalty {
        body["repeat_penalty"] = json!(p);
    }
    if let Some(p) = request.frequency_penalty {
        body["frequency_penalty"] = json!(p);
    }
}

/// The host's `choices[0].logprobs` for a request that asked for them. A host
/// that reported none is traced, so the absence is visible where it happened.
pub(crate) fn answered_logprobs(
    request: &CompletionRequest,
    reported: Option<&Option<Value>>,
) -> Option<Value> {
    let asked = request.logprobs?;
    let answered = reported.cloned().flatten().filter(|v| !v.is_null());
    if answered.is_none() {
        tracing::debug!(
            target: "oicp_client",
            top_logprobs = asked,
            "logprobs asked, the host reported none"
        );
    }
    answered
}

/// The `enable_thinking` `request` is rendered with, as the engine renders
/// it. A conversation goes to the model's own template, which decides when
/// the caller did not (thinking on for a thinking template, on both sides:
/// `prompt.conversation`). Any other shape is rendered with
/// [`CompletionRequest::templated_thinking`], off unless asked for, and the
/// host is told so: llama-server would otherwise open thinking for a
/// thinking template (LS server-context.cpp:1377-1381), where the engine has
/// it closed (`prompt.templated-thinking-default`).
pub(crate) fn template_thinking(request: &CompletionRequest) -> Option<bool> {
    match request.prompt_shape {
        Some(PromptShape::Conversation { .. }) => request.enable_thinking,
        _ => Some(request.templated_thinking()),
    }
}

/// `schema` with `title` set to `label`, so a host that takes the schema as a
/// function call names the function after it. No label, no change.
pub fn titled_schema(schema: &Value, label: Option<&str>) -> Value {
    let mut out = schema.clone();
    if let (Some(label), Some(obj)) = (label, out.as_object_mut()) {
        obj.insert("title".into(), json!(label));
    }
    out
}

/// An OpenAI function or schema name must match `^[a-zA-Z0-9_-]{1,64}$`; a
/// schema label is free text, so fold it rather than let the host 400 on it.
pub fn openai_function_name(name: &str) -> String {
    let folded: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if folded.is_empty() {
        "emit_response".to_string()
    } else {
        folded
    }
}

/// The name a schema goes by on the wire: its `title`, folded.
pub(crate) fn schema_function_name(schema: &Value) -> String {
    let title = schema.get("title").and_then(Value::as_str);
    openai_function_name(title.unwrap_or("structured"))
}

impl RemoteApiProvider {
    /// The host's spelling now: as configured, or a forced function call once
    /// the host has refused `json_schema` and answered that instead.
    pub(crate) fn effective_structured_output_mode(&self) -> StructuredOutputMode {
        if self.json_schema_refused.load(Ordering::Relaxed) {
            StructuredOutputMode::ToolUseForced
        } else {
            self.structured_output_mode
        }
    }

    /// POST one chat completion in this host's spelling, and say which
    /// spelling answered.
    ///
    /// A host that rejects `json_schema` (DeepSeek's chat API answers 400
    /// "unavailable now") is asked again, once, with the schema as a forced
    /// function call. Only a retry that ANSWERS flips the provider for the rest
    /// of its life: a 400 for another reason (an oversized prompt) fails the
    /// retry too, and both refusals are reported. Streaming does not retry; it
    /// follows the flip once a completion has learned it.
    ///
    /// Only a LAST-RESORT endpoint is asked again (`wait_out_sheds`, the same
    /// persist-or-report decision as a shed). A peer's refusal is a routing
    /// signal and goes back to the router at once, unchanged.
    pub(crate) async fn send_chat(
        &self,
        admitted: &crate::outbound::Admitted<'_>,
        url: &str,
        request: &CompletionRequest,
    ) -> Result<(reqwest::Response, StructuredOutputMode)> {
        let what = "Remote API request";
        let mode = self.effective_structured_output_mode();
        let body = self.build_request_in(request, mode)?;
        // The whole body, so a run can be replayed by hand (§9.1). Debug: it
        // carries the full prompt and any schema.
        tracing::debug!(target: "oicp_client", %url, %body, "chat request body");
        observe_wire_request(&body);
        let send = |b: &Value| admitted.post(url).json(b);
        let refusal = match self.send_honouring_shed_raw(|| send(&body), what).await? {
            Ok(response) => return Ok((response, mode)),
            Err(refusal) => refusal,
        };
        let schema_refused = self.wait_out_sheds
            && request.structured_output.is_some()
            && mode == StructuredOutputMode::JsonSchema
            && refusal.status == reqwest::StatusCode::BAD_REQUEST;
        if !schema_refused {
            return Err(refusal.into_error(what));
        }
        let forced = StructuredOutputMode::ToolUseForced;
        let retry = self.build_request_in(request, forced)?;
        tracing::info!(
            target: "oicp_client",
            %url,
            refusal = %error_excerpt(&refusal.body),
            "host refused a json_schema request (400); asking once more with the schema as a forced function call"
        );
        tracing::debug!(target: "oicp_client", %url, body = %retry, "chat request body");
        observe_wire_request(&retry);
        match self.send_honouring_shed_raw(|| send(&retry), what).await? {
            Ok(response) => {
                self.json_schema_refused.store(true, Ordering::Relaxed);
                tracing::warn!(
                    target: "oicp_client",
                    %url,
                    "json_schema refused, forced function call answered: this provider sends schemas as a function call from now on"
                );
                Ok((response, forced))
            }
            Err(second) => Err(Error::Inference(format!(
                "{what} returned {}: {}; asked again with the schema as a forced function call, it returned {}: {}",
                refusal.status,
                error_excerpt(&refusal.body),
                second.status,
                error_excerpt(&second.body)
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
