// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two provider wire formats.
//!
//! `DaemonInferenceClient` speaks OpenAI-compatible chat completions and the
//! Anthropic messages API, and each is a few hundred lines of request shaping,
//! streaming, retry and error mapping. They are the same method twice in two
//! dialects, so they live together and apart from the client's own lifecycle.

use crate::providers::{
    local_daemon_base, parse_model_spec, ProviderKind, ProviderRegistry, ResolvedProvider,
};
use corpus_engine::enrichment::pipeline::{
    ChatCompletionFn, ChatCompletionWithTokensFn, ChatPrompt,
};
use corpus_engine::error::{Error, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::DaemonInferenceClient;

impl DaemonInferenceClient {
    /// Original OpenAI-shape `/v1/chat/completions` dispatch. Kept
    /// byte-identical to the pre-multi-provider behavior so the
    /// local daemon path is unchanged.
    pub(super) async fn complete_openai_compatible(
        &self,
        provider: &ResolvedProvider,
        model: &str,
        prompt: &ChatPrompt,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        // base_url carries the API version (e.g. `.../v1`); dispatcher
        // only appends the dialect-specific endpoint suffix. This
        // keeps the convention consistent across vN releases — bump
        // base_url when a provider ships a v2 endpoint.
        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/'),
        );
        // `think_budget: 0` instructs the daemon to inject `/no_think`
        // for SystemPromptToken thinking families (Qwen3 / Qwen3.5 /
        // SmolLM3). The schema constraint already forces JSON
        // correctness for atlas Phase 1; chain-of-thought tokens are
        // pure latency cost — Qwen3.5-4B with thinking disabled went
        // from 60+ s/chapter to ~10 s/chapter on the wiki-tier2-bank
        // run. Models without SystemPromptToken thinking control
        // (Gemma 3/4, Llama 3, Phi-4) ignore this field harmlessly.
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
        // default → 0 (suppress for SystemPromptToken families like
        // Qwen3/Qwen3.5/SmolLM3 where it costs latency without
        // benefit at the schema-bound Phase 1).
        let think_budget = prompt
            .thinking_tokens
            .or(provider.default_thinking_tokens)
            .unwrap_or(0);
        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": prompt.system},
                {"role": "user", "content": prompt.user},
            ],
            "temperature": temperature,
            "stream": false,
            "think_budget": think_budget,
        });
        // DeepSeek-style thinking control. V3.1+ / V4 reject unknown
        // dialect-mismatched fields silently, so this is safe for
        // other OpenAI-compat providers — they ignore it. Local
        // daemon sees both `think_budget` (its native field) and
        // `thinking` (no-op).
        //   `{"type":"disabled"}` — fully suppress reasoning
        //   `{"type":"enabled"}` + optional `budget_tokens` — opt in
        if let Some(obj) = body.as_object_mut() {
            let thinking = if think_budget == 0 {
                serde_json::json!({"type": "disabled"})
            } else {
                serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": think_budget,
                })
            };
            obj.insert("thinking".into(), thinking);
        }
        if let Some(n) = max_tokens {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("max_tokens".into(), serde_json::json!(n));
            }
        }
        // OICP-v0.3 routing: when the composer attached an explicit
        // `prompt.max_output_tokens` it has opted into hard-gated
        // claim selection (per spec §2.4). Surface the budget — and
        // pin latency to Fast so the request lands on a FastShort/
        // FastLong claim rather than getting deprioritized to a
        // Normal-latency Slow slot. Composers without an explicit
        // budget keep the existing model-name routing path.
        if let Some(mo) = prompt.max_output_tokens {
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "oicp".into(),
                    serde_json::json!({
                        // Required by the daemon's request validator
                        // (oicp-types §1). Without it the request
                        // 422s before reaching the model — silently
                        // killing phase1b entity/concept coverage
                        // passes, which is how MacIntyre / Sandel /
                        // Walzer leaked through Phase 1 extraction
                        // even though the coverage prompt was
                        // designed to catch them.
                        "oicp_version": "0.3.0",
                        "max_output_tokens": mo,
                        "latency_class": "fast",
                    }),
                );
            }
        }
        // Structured-output mode is provider-configurable. Default
        // for OpenAI-compat is `json_schema` (full grammar
        // enforcement). DeepSeek's chat-completions only honors
        // `json_object`, so providers configured for DeepSeek set
        // `structured_output_mode = "json-object"` and we drop the
        // schema — the prompt itself carries the schema as text and
        // the post-parser tolerates drift.
        let mut has_schema = false;
        if let Some(schema) = prompt.response_schema.as_ref() {
            let name = prompt
                .response_schema_name
                .as_deref()
                .unwrap_or("response_schema");
            use crate::providers::StructuredOutputMode as M;
            let response_format = match provider.structured_output_mode {
                M::JsonSchema => Some(serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": name,
                        "schema": schema,
                        "strict": true
                    }
                })),
                M::JsonObject => Some(serde_json::json!({"type": "json_object"})),
                // tool-use modes don't apply to OpenAI-compat path —
                // they're an Anthropic-shape construct. Fall through
                // to json_object as the safest weak-enforcement
                // option.
                M::ToolUseAuto | M::ToolUseForced => {
                    Some(serde_json::json!({"type": "json_object"}))
                }
            };
            if let (Some(obj), Some(rf)) = (body.as_object_mut(), response_format) {
                obj.insert("response_format".into(), rf);
                has_schema = true;
            }
        }

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

        tracing::info!(
            phase = %phase_label,
            model = %model_label,
            schema = has_schema,
            max_tokens = ?max_tokens,
            "inference_client: dispatching /v1/chat/completions"
        );

        // Bearer auth for remote OpenAI-compatible providers
        // (OpenAI, OpenRouter, Together, vLLM-with-auth). The local
        // daemon's `auth_secret` is `None`, so this is a no-op there.
        let mut request_builder = self.client.post(&url).json(&body);
        if let Some(secret) = provider.auth_secret.as_deref() {
            request_builder = request_builder.bearer_auth(secret);
        }
        let result = request_builder.send().await;
        let outcome = match result {
            Ok(resp) => {
                let status = resp.status();
                let text_res = resp.text().await;
                match text_res {
                    Ok(text) if !status.is_success() => Err(Error::Serialization(format!(
                        "daemon chat error {status}: {text}"
                    ))),
                    Ok(text) => Ok(text),
                    Err(e) => Err(Error::Serialization(format!(
                        "chat response read error: {e}"
                    ))),
                }
            }
            Err(e) => Err(Error::from(e)),
        };

        heartbeat.abort();

        let elapsed_ms = started.elapsed().as_millis();
        let text = match outcome {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    phase = %phase_label,
                    model = %model_label,
                    elapsed_ms = elapsed_ms as u64,
                    error = %e,
                    "inference_client: /v1/chat/completions failed"
                );
                return Err(e);
            }
        };
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            Error::Serialization(format!("non-JSON chat response: {e} — body: {text}"))
        })?;
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(|s| s.as_str())
            .ok_or_else(|| {
                Error::Serialization(format!(
                    "chat response missing choices[0].message.content: {text}"
                ))
            })?;
        let total_tokens = v
            .pointer("/usage/total_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let prompt_tokens = v
            .pointer("/usage/prompt_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let completion_tokens = v
            .pointer("/usage/completion_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
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
        // Extract finish_reason from choices[0].finish_reason. Standard
        // OpenAI shape: "stop" (EOS), "length" (max_tokens), "tool_calls".
        // Added 2026-05-17 during the SEP-pipeline profiling pass —
        // distinguishing EOS vs Length is essential for diagnosing
        // truncated-output failures (we were seeing 361-token completions
        // with no signal whether the model hit EOS or the daemon clamped
        // max_tokens). Daemon-side population: see
        // `sovereign_mesh::inference_adapter::translate_finish_reason`.
        let finish_reason = v
            .pointer("/choices/0/finish_reason")
            .and_then(|s| s.as_str())
            .unwrap_or("?")
            .to_string();
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
        Ok(content.to_string())
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
        let outcome = match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(text) if !status.is_success() => Err(Error::Serialization(format!(
                        "anthropic chat error {status}: {text}"
                    ))),
                    Ok(text) => Ok(text),
                    Err(e) => Err(Error::Serialization(format!(
                        "anthropic response read error: {e}"
                    ))),
                }
            }
            Err(e) => Err(Error::from(e)),
        };
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
