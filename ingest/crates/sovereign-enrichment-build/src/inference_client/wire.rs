// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two provider wire formats.
//!
//! `DaemonInferenceClient` speaks OpenAI-compatible chat completions and the
//! Anthropic messages API, and each is a few hundred lines of request shaping,
//! streaming, retry and error mapping. They are the same method twice in two
//! dialects, so they live together and apart from the client's own lifecycle.

use crate::providers::ResolvedProvider;
use corpus_engine::enrichment::pipeline::ChatPrompt;
use corpus_engine::error::{Error, Result};
use oicp_client::{titled_schema, RemoteApiProvider};
use oicp_types::tool_calls::parse_tool_calls_from_text;
use oicp_types::{CompletionRequest, InferenceRequirements, LatencyClass};
use sovereign_contracts::traits::InferenceProvider;
use std::sync::atomic::Ordering;

use super::DaemonInferenceClient;

impl DaemonInferenceClient {
    /// OpenAI-shape `/v1/chat/completions` dispatch through the one chat
    /// builder (`oicp_client::RemoteApiProvider`): the body, the shed loop and
    /// the response read live there, shared with the daemon's mesh hop. What
    /// stays here is enrich's own: its request defaults, the heartbeat, the
    /// token ledger.
    pub(super) async fn complete_openai_compatible(
        &self,
        provider: &ResolvedProvider,
        model: &str,
        prompt: &ChatPrompt,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        // Temperature precedence:
        //   1. per-prompt (composer-attached, atlas phase override)
        //   2. provider default (e.g. anthropic config)
        //   3. dispatcher hardcoded fallback (0.2 — matches the
        //      historical pre-multi-provider default)
        let temperature = prompt
            .temperature
            .or(provider.default_temperature)
            .unwrap_or(0.2);
        // Thinking budget precedence: prompt override → provider
        // default → 0. Zero suppresses thinking for SystemPromptToken
        // families (Qwen3 / Qwen3.5 / SmolLM3): the schema constraint
        // already forces JSON correctness for atlas Phase 1, so
        // chain-of-thought is pure latency — Qwen3.5-4B went from 60+
        // s/chapter to ~10 s/chapter on the wiki-tier2-bank run.
        let think_budget = prompt
            .thinking_tokens
            .or(provider.default_thinking_tokens)
            .unwrap_or(0);
        let mode = provider.structured_output_mode;
        let has_schema = prompt.response_schema.is_some();
        let via_tool = has_schema && mode.via_tool();
        let request = CompletionRequest {
            prompt: prompt.user.clone(),
            system_message: Some(prompt.system.clone()),
            model_id: Some(model.to_string()),
            temperature: Some(temperature),
            max_tokens: max_tokens.map(|n| n as usize),
            think_budget: Some(think_budget as usize),
            // A zero budget for the chat template too: the one spelling a
            // bare llama-server reads (`chat_wire::write_thinking`).
            enable_thinking: (think_budget == 0).then_some(false),
            // The schema label rides as the schema's `title`, which a
            // function-call host names the function after.
            structured_output: prompt
                .response_schema
                .as_ref()
                .map(|s| titled_schema(s, prompt.response_schema_name.as_deref())),
            // OICP routing: a composer that attached an explicit
            // `max_output_tokens` has opted into hard-gated claim selection
            // (§2.4), pinned to Fast so it lands on a FastShort/FastLong claim
            // rather than a Normal-latency Slow slot. Without the envelope the
            // phase1b coverage passes 422'd at the daemon's validator, which
            // is how MacIntyre / Sandel / Walzer leaked through Phase 1.
            oicp: prompt.max_output_tokens.map(|mo| {
                InferenceRequirements::new()
                    .with_latency_class(LatencyClass::Fast)
                    .with_max_output_tokens(mo)
            }),
            ..Default::default()
        };
        // Chat only, so the context size is never read on this path.
        let host = RemoteApiProvider::with_client(
            &provider.base_url,
            self.client.clone(),
            provider.auth_secret.clone(),
            model,
            0,
        )
        .originating()
        .waiting_out_sheds()
        .with_structured_output_mode(mode)
        .with_extra_params(provider.extra_params.clone());

        // Observability: a Phase-1 chat call against the fast slot
        // routinely runs minutes. Without a heartbeat the CLI looks
        // dead from the outside — operator can't tell "daemon is still
        // generating" from "daemon wedged on a grammar mask". Spawn a
        // 15s ticker that emits a stderr line tagged with phase_id +
        // model + elapsed; cancel it as soon as the response lands.
        // tracing::info also goes through the subscriber so
        // RUST_LOG=info upgrades the heartbeat to richer context.
        let started = std::time::Instant::now();
        let phase_label = prompt.phase_id.clone().unwrap_or_else(|| "?".to_string());
        let model_label = model.to_string();
        let heartbeat = {
            let phase = phase_label.clone();
            let model = model_label.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
                tick.tick().await; // skip immediate
                loop {
                    tick.tick().await;
                    let elapsed = started.elapsed().as_secs();
                    eprintln!(
                        "      · waiting on daemon ({elapsed}s elapsed, phase={phase}, model={model}, schema={has_schema})"
                    );
                    tracing::info!(
                        elapsed_s = elapsed,
                        phase = %phase,
                        model = %model,
                        schema = has_schema,
                        "inference_client: still waiting on /v1/chat/completions"
                    );
                }
            })
        };

        // The request body itself is traced at debug by `oicp_client`.
        tracing::info!(
            phase = %phase_label,
            model = %model_label,
            schema = has_schema,
            mode = ?mode,
            max_tokens = ?max_tokens,
            "inference_client: dispatching /v1/chat/completions"
        );
        let outcome = host.complete(&request).await;
        heartbeat.abort();

        let elapsed_ms = started.elapsed().as_millis();
        let resp = match outcome {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    phase = %phase_label,
                    model = %model_label,
                    elapsed_ms = elapsed_ms as u64,
                    error = %e,
                    "inference_client: /v1/chat/completions failed"
                );
                return Err(Error::Serialization(format!(
                    "chat error from `{}`: {e}",
                    provider.name
                )));
            }
        };
        // A forced function call comes back as the `<tool_call>` envelope
        // `oicp_client` renders native calls into; its arguments are the answer.
        let tool_args = via_tool
            .then(|| parse_tool_calls_from_text(&resp.text).into_iter().next())
            .flatten()
            .map(|call| call.arguments);
        let text_content = Some(resp.text.as_str()).filter(|s| !s.trim().is_empty());
        let content = match (via_tool, tool_args, text_content) {
            (true, Some(args), _) => args,
            // `tool_choice: auto` lets the model answer in text instead. Use
            // it, and say so: the schema was offered, not enforced.
            (true, None, Some(t)) => {
                tracing::warn!(
                    phase = %phase_label,
                    model = %model_label,
                    "inference_client: tool offered but the model answered in text; \
                     parsing the text, schema not enforced on this call"
                );
                t.to_string()
            }
            (false, _, Some(t)) => t.to_string(),
            _ => {
                return Err(Error::Serialization(format!(
                    "chat response from `{}` carried neither content nor a tool call \
                     (finish_reason {:?})",
                    provider.name, resp.finish_reason
                )))
            }
        };
        // The answer as the parser will see it, beside the request body
        // `oicp_client` logged, so a parse failure can be diagnosed from one
        // run (§9.1).
        tracing::debug!(
            phase = %phase_label,
            via_tool,
            content = %content,
            "inference_client: response content"
        );
        let total_tokens = resp.tokens_used as u64;
        let prompt_tokens = resp.prompt_tokens as u64;
        let completion_tokens = resp.completion_tokens.unwrap_or(0) as u64;
        // Phase D2: bump the cumulative ledger. Relaxed ordering is
        // sufficient — we never branch on these counts, just persist
        // them periodically for status display.
        self.usage.calls.fetch_add(1, Ordering::Relaxed);
        self.usage
            .prompt_tokens
            .fetch_add(prompt_tokens, Ordering::Relaxed);
        self.usage
            .completion_tokens
            .fetch_add(completion_tokens, Ordering::Relaxed);
        self.usage
            .total_tokens
            .fetch_add(total_tokens, Ordering::Relaxed);
        let tok_per_s = if elapsed_ms > 0 {
            (completion_tokens as f64 * 1000.0) / (elapsed_ms as f64)
        } else {
            0.0
        };
        // "stop" (EOS), "length" (max_tokens), "tool_calls". Distinguishing
        // EOS from Length is what diagnoses truncated output (2026-05-17:
        // 361-token completions with no signal which one ended them).
        // Daemon-side population:
        // `sovereign_serving_host::inference_adapter::translate_finish_reason`.
        let finish_reason = resp
            .finish_reason
            .as_ref()
            .map_or("?", |f| f.as_openai_str());
        tracing::info!(
            phase = %phase_label,
            model = %model_label,
            elapsed_ms = elapsed_ms as u64,
            total_tokens,
            completion_tokens,
            tok_per_s = format!("{tok_per_s:.1}"),
            finish_reason = %finish_reason,
            "inference_client: /v1/chat/completions ok"
        );
        Ok(content)
    }

    /// Anthropic `/v1/messages` dispatch. Translates the OpenAI-shape
    /// `ChatPrompt` (`system` + `user` strings + optional JSON Schema)
    /// into Anthropic's native shape:
    ///   - `system` is a top-level field, not a message
    ///   - `tools` carries the JSON Schema as an `input_schema`, with
    ///     `tool_choice = {"type":"tool","name":"emit_response"}` to
    ///     force the model to call our extraction tool
    ///   - `thinking` block enables extended thinking when
    ///     `default_thinking_tokens` > 0
    /// Token usage is reported in `usage.input_tokens` /
    /// `usage.output_tokens` (different field names than OpenAI).
    pub(super) async fn complete_anthropic(
        &self,
        provider: &ResolvedProvider,
        model: &str,
        prompt: &ChatPrompt,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        let url = format!("{}/messages", provider.base_url.trim_end_matches('/'),);
        // Temperature precedence: prompt → provider → 0.2 fallback.
        let temperature = prompt
            .temperature
            .or(provider.default_temperature)
            .unwrap_or(0.2);
        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": max_tokens.unwrap_or(4096),
            "system": prompt.system,
            "messages": [
                {"role": "user", "content": prompt.user},
            ],
            "temperature": temperature,
        });
        // Structured-output mode: when the prompt carries a JSON
        // Schema, expose it as an Anthropic tool with
        // `input_schema = <schema>` and force the model to call it.
        // The dispatcher then unwraps the tool_use block back into a
        // JSON string, matching the contract the rest of the
        // pipeline expects (response = JSON content of the schema).
        let mut has_schema = false;
        if let Some(schema) = prompt.response_schema.as_ref() {
            let tool_name = prompt
                .response_schema_name
                .as_deref()
                .unwrap_or("emit_response");
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "tools".into(),
                    serde_json::json!([{
                        "name": tool_name,
                        "description": "Emit the structured response matching the provided JSON schema.",
                        "input_schema": schema,
                    }]),
                );
                // tool_choice="auto" instead of forced
                // ({"type":"tool",...}) so the request works on
                // models like DeepSeek-reasoner that reject forced
                // tool selection. Anthropic's own models still
                // voluntarily call when the system prompt directs
                // them to, so the practical recall is the same.
                // Trade-off: model can in principle decline to call
                // and emit text — the response unwrapper falls
                // through to text-block aggregation in that case.
                obj.insert("tool_choice".into(), serde_json::json!({"type": "auto"}));
                has_schema = true;
            }
        }
        // Extended thinking precedence: prompt-level override (per-
        // phase, set by the atlas operator) wins over provider
        // default. `Some(0)` is explicit "thinking disabled" — we
        // just don't send the field. `None` inherits provider
        // default; if that's also `None` or 0, no thinking.
        let thinking_budget = prompt
            .thinking_tokens
            .or(provider.default_thinking_tokens)
            .unwrap_or(0);
        if thinking_budget > 0 {
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "thinking".into(),
                    serde_json::json!({
                        "type": "enabled",
                        "budget_tokens": thinking_budget,
                    }),
                );
            }
        }
        // Vendor passthroughs (extra_params) merged last — operator
        // can pin top_k or other niche knobs without dispatcher
        // changes.
        if let Some(extra) = provider.extra_params.as_ref() {
            if let (Some(obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
                for (k, v) in extra_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }

        let started = std::time::Instant::now();
        let phase_label = prompt.phase_id.clone().unwrap_or_else(|| "?".to_string());
        let model_label = model.to_string();
        let provider_label = provider.name.clone();
        let heartbeat = {
            let phase = phase_label.clone();
            let model = model_label.clone();
            let provider = provider_label.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
                tick.tick().await;
                loop {
                    tick.tick().await;
                    let elapsed = started.elapsed().as_secs();
                    eprintln!(
                        "      · waiting on {provider} ({elapsed}s elapsed, phase={phase}, model={model}, schema={has_schema})"
                    );
                    tracing::info!(
                        elapsed_s = elapsed,
                        provider = %provider,
                        phase = %phase,
                        model = %model,
                        schema = has_schema,
                        "inference_client: still waiting on Anthropic /v1/messages"
                    );
                }
            })
        };
        tracing::info!(
            provider = %provider_label,
            phase = %phase_label,
            model = %model_label,
            schema = has_schema,
            max_tokens = ?max_tokens,
            "inference_client: dispatching Anthropic /v1/messages"
        );

        let api_version = provider.api_version.as_deref().unwrap_or("2023-06-01");
        let mut req = self
            .client
            .post(&url)
            .json(&body)
            .header("anthropic-version", api_version);
        if let Some(secret) = provider.auth_secret.as_deref() {
            req = req.header("x-api-key", secret);
        } else {
            heartbeat.abort();
            return Err(Error::Serialization(format!(
                "anthropic provider `{}` has no api_key_env configured (or the env var is empty)",
                provider.name
            )));
        }
        let outcome = send_honouring_shed(&req, "anthropic chat", &phase_label)
            .await
            .and_then(|(status, text)| {
                if status.is_success() {
                    Ok(text)
                } else {
                    Err(Error::Serialization(format!(
                        "anthropic chat error {status}: {text}"
                    )))
                }
            });

        heartbeat.abort();

        let elapsed_ms = started.elapsed().as_millis();
        let text = match outcome {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    provider = %provider_label,
                    phase = %phase_label,
                    model = %model_label,
                    elapsed_ms = elapsed_ms as u64,
                    error = %e,
                    "inference_client: Anthropic /v1/messages failed"
                );
                return Err(e);
            }
        };
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            Error::Serialization(format!("non-JSON anthropic response: {e} — body: {text}"))
        })?;
        // Anthropic response shape: {content: [{type, text|input}, ...]}.
        // For schema-constrained calls we pulled a `tool_use` block;
        // for free-form calls we pulled a `text` block.
        let content = v
            .pointer("/content")
            .and_then(|c| c.as_array())
            .ok_or_else(|| {
                Error::Serialization(format!("anthropic response missing content[]: {text}"))
            })?;
        let extracted = if has_schema {
            // tool_choice=auto: model usually calls the tool but
            // may decline. Try tool_use first; on miss, fall through
            // to text aggregation so the caller still gets the raw
            // JSON the model emitted (often valid given the
            // system prompt's structured-output instructions).
            if let Some(tool_use) = content
                .iter()
                .find(|b| b.pointer("/type").and_then(|t| t.as_str()) == Some("tool_use"))
            {
                let input = tool_use.pointer("/input").ok_or_else(|| {
                    Error::Serialization(format!("anthropic tool_use block missing /input: {text}"))
                })?;
                serde_json::to_string(input).map_err(|e| {
                    Error::Serialization(format!("re-serialize anthropic tool input: {e}"))
                })?
            } else {
                // No tool_use — the model emitted text instead.
                // Concatenate text blocks and trust downstream
                // JSON-tolerant parsers to handle the response.
                content
                    .iter()
                    .filter_map(|b| {
                        if b.pointer("/type").and_then(|t| t.as_str()) == Some("text") {
                            b.pointer("/text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            }
        } else {
            // First text block; concatenate if multiple.
            content
                .iter()
                .filter_map(|b| {
                    if b.pointer("/type").and_then(|t| t.as_str()) == Some("text") {
                        b.pointer("/text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        };

        let prompt_tokens = v
            .pointer("/usage/input_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let completion_tokens = v
            .pointer("/usage/output_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let total_tokens = prompt_tokens + completion_tokens;
        self.usage.calls.fetch_add(1, Ordering::Relaxed);
        self.usage
            .prompt_tokens
            .fetch_add(prompt_tokens, Ordering::Relaxed);
        self.usage
            .completion_tokens
            .fetch_add(completion_tokens, Ordering::Relaxed);
        self.usage
            .total_tokens
            .fetch_add(total_tokens, Ordering::Relaxed);
        let tok_per_s = if elapsed_ms > 0 {
            (completion_tokens as f64 * 1000.0) / (elapsed_ms as f64)
        } else {
            0.0
        };
        tracing::info!(
            provider = %provider_label,
            phase = %phase_label,
            model = %model_label,
            elapsed_ms = elapsed_ms as u64,
            total_tokens,
            completion_tokens,
            tok_per_s = format!("{tok_per_s:.1}"),
            "inference_client: Anthropic /v1/messages ok"
        );
        Ok(extracted)
    }
}

/// Send a request, honouring a daemon shed rather than reporting it as a
/// failure. The DECISION — is this 503 a shed, and for how long — belongs to
/// [`oicp_client::shed_retry_after`], the one decider (§10.6); this function
/// owns only the loop and the tracing.
///
/// Before this existed, every non-success status became an error here, and
/// Phase 6 lost 5 of 65 tension candidates to `local_queue_full` on two
/// consecutive builds (2026-09-02) against a body that named the wait.
///
/// Returns the final `(status, body)` — including a non-success one once the
/// budget is spent — so each caller keeps its own error wording.
pub(super) async fn send_honouring_shed(
    request: &reqwest::RequestBuilder,
    what: &str,
    phase: &str,
) -> Result<(reqwest::StatusCode, String)> {
    let mut waited = std::time::Duration::ZERO;
    let mut attempt = 0u32;
    loop {
        let send = request
            .try_clone()
            .expect("a JSON-bodied request is cloneable")
            .send()
            .await;
        let resp = match send {
            Ok(r) => r,
            Err(e) => return Err(Error::from(e)),
        };
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Serialization(format!("{what} response read error: {e}")))?;
        if status.is_success() {
            return Ok((status, body));
        }
        attempt += 1;
        let Some(delay) = oicp_client::shed_retry_after(status, &body) else {
            return Ok((status, body));
        };
        if attempt >= oicp_client::SHED_MAX_ATTEMPTS
            || waited + delay > oicp_client::SHED_TOTAL_WAIT_CAP
        {
            tracing::warn!(
                what,
                phase,
                attempt,
                waited_s = waited.as_secs(),
                "inference_client: shed budget spent; reporting the host's refusal"
            );
            return Ok((status, body));
        }
        tracing::warn!(
            what,
            phase,
            attempt,
            delay_s = delay.as_secs(),
            "inference_client: host shed the request; honouring the wait it asked for"
        );
        tokio::time::sleep(delay).await;
        waited += delay;
    }
}
