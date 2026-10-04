// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat dispatch: enrich's request defaults, heartbeat and token ledger
//! around the one OpenAI-compatible builder (`oicp_client::RemoteApiProvider`),
//! apart from the client's own lifecycle.

use corpus_engine::enrichment::pipeline::ChatPrompt;
use corpus_engine::error::{Error, Result};
use oicp_client::{titled_schema, RemoteApiProvider};
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
        model: &str,
        prompt: &ChatPrompt,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        // Temperature: per-prompt (composer-attached, atlas phase override),
        // else 0.2.
        let temperature = prompt.temperature.unwrap_or(0.2);
        // Thinking budget: per-prompt, else 0. Zero suppresses thinking for
        // SystemPromptToken families (Qwen3 / Qwen3.5 / SmolLM3): the schema
        // constraint already forces JSON correctness for atlas Phase 1, so
        // chain-of-thought is pure latency — Qwen3.5-4B went from 60+
        // s/chapter to ~10 s/chapter on the wiki-tier2-bank run.
        let think_budget = prompt.thinking_tokens.unwrap_or(0);
        let mode = self.structured_output_mode;
        let has_schema = prompt.response_schema.is_some();
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
        let host =
            RemoteApiProvider::with_client(&self.chat_base, self.client.clone(), None, model, 0)
                .originating()
                .waiting_out_sheds()
                .with_structured_output_mode(mode);

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
                    "chat error from {}: {e}",
                    self.chat_base
                )));
            }
        };
        // The JSON answer whichever spelling the host needed: a schema that
        // rode a function call comes back as its arguments (`oicp_client`).
        if resp.text.trim().is_empty() {
            return Err(Error::Serialization(format!(
                "chat response from {} carried neither content nor a tool call \
                 (finish_reason {:?})",
                self.chat_base, resp.finish_reason
            )));
        }
        let content = resp.text;
        // The answer as the parser will see it, beside the request body
        // `oicp_client` logged, so a parse failure can be diagnosed from one
        // run (§9.1).
        tracing::debug!(
            phase = %phase_label,
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
