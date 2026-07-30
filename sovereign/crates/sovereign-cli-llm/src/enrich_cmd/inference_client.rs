// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP client glue for talking to the Commonwealth daemon's
//! OpenAI-compatible chat + embeddings endpoints.
//!
//! This is the only place in `enrich_cmd/` that knows about
//! reqwest and the wire shape — every other subcommand just
//! takes the pair of closures (`EmbedFn` + `ChatCompletionFn`)
//! produced by `build_client_pair`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::enrichment::pipeline::{
    ChatCompletionFn, ChatCompletionWithTokensFn, ChatPrompt,
};
use corpus_engine::error::{Error, Result};
use corpus_engine::types::EmbedFn;

use super::providers::{parse_model_spec, ProviderKind, ProviderRegistry, ResolvedProvider};

/// Default chat request timeout. Phase 1 extract on a 27B-Q6 model
/// emitting up to 16k tokens of structured JSON can run 5–15 minutes
/// on M2 hardware. The previous 180s ceiling silently killed real
/// SEP campaign requests (verified 2026-04-25 — 14kB request took
/// 168s; full sections take far longer). When reqwest's timeout
/// fires the daemon keeps the inference slot, so subsequent
/// requests pile up against a held lock — a cascading failure
/// that looks like the daemon itself is wedged. 1800s leaves
/// generous headroom; if the request really is stuck, the user
/// will notice on a 30-minute hang.
const CHAT_TIMEOUT: Duration = Duration::from_secs(1800);

/// Default embed request timeout. Embeddings are fast; we keep this
/// tight so a hung embed surface doesn't freeze a whole run.
const EMBED_TIMEOUT: Duration = Duration::from_secs(15);

/// Reusable OpenAI-compatible chat client pointed at the local daemon.
#[derive(Debug, Clone)]
pub struct DaemonInferenceClient {
    client: reqwest::Client,
    base_url: String,
    chat_model: String,
    embed_model: String,
    /// Per-request output token cap. `None` means "let the daemon
    /// decide" — which on some llama.cpp builds means 256, too small
    /// for thinking models. Callers that load `EnrichConfig` should
    /// thread its `max_output_tokens` through via
    /// `with_max_output_tokens`.
    max_output_tokens: Option<u32>,
    /// Per-phase chat-model overrides. When a `ChatPrompt` arrives
    /// tagged with `phase_id`, the client looks up the phase here and
    /// — if a matching entry exists — sends the request with that
    /// model id instead of `chat_model`. Empty map (the default)
    /// means "always use `chat_model`", preserving the historical
    /// single-model behaviour.
    chat_models_by_phase: BTreeMap<String, String>,
    /// Per-phase output-token cap override. When a `ChatPrompt` arrives
    /// tagged with a `phase_id` present in this map, the client uses
    /// the mapped value as `max_tokens` for that request, instead of
    /// the global `max_output_tokens`. Empty map (the default) means
    /// every phase uses the global cap.
    ///
    /// Used to bound Phase 1b (entity / concept coverage). Those
    /// passes run without a JSON-Schema constraint, so models with
    /// thinking disabled (Qwen3 / Qwen3.5 with `/no_think`) elaborate
    /// freely and routinely consume the entire 2048-token Phase-1
    /// budget per pass — adding ~30s per chapter for limited extra
    /// signal. A 1024 cap halves that without affecting the
    /// schema-bound Phase 1 main.
    max_tokens_by_phase: BTreeMap<String, u32>,
    /// Per-phase request-shape overrides (temperature, thinking).
    /// Mirrors `PhaseOverride` from EnrichConfig; the client applies
    /// matching entries to every outgoing prompt's `phase_id` before
    /// dispatch. Empty map = no per-phase tuning, fall through to
    /// provider defaults.
    phase_overrides: BTreeMap<String, super::config::PhaseOverride>,
    /// Phase D2 — token ledger. Atomic counters bumped on every
    /// successful `complete_inner` call. Cloned across `Clone`d
    /// clients (Arc-wrapped), so the extract loop sees a unified
    /// total even when `EmbedFn` / `ChatCompletionFn` closures are
    /// constructed from a clone (`build_client_pair` does this).
    /// Cheap (relaxed atomics; no lock) so the hot path stays hot.
    usage: Arc<TokenUsageLedger>,
    /// Multi-provider registry. Holds the built-in `local` provider
    /// pointed at `base_url`, plus any operator-configured remote
    /// providers from `~/.config/sovereign/providers.toml`. The
    /// dispatcher resolves `provider:model` syntax in the configured
    /// `chat_model` (or per-phase override) by looking up here. Bare
    /// model ids without a `provider:` prefix route to `local`,
    /// preserving prior behavior.
    providers: Arc<ProviderRegistry>,
}

/// Cumulative token usage for a chat client. Atomic counters keep
/// the bump operation cheap on the hot path; the read-side
/// `snapshot` returns a plain struct for serialisation.
#[derive(Debug, Default)]
pub struct TokenUsageLedger {
    pub calls: AtomicU64,
    pub prompt_tokens: AtomicU64,
    pub completion_tokens: AtomicU64,
    pub total_tokens: AtomicU64,
}

impl TokenUsageLedger {
    /// Snapshot the four counters with relaxed loads. Safe to call
    /// from any thread; the snapshot is internally consistent only
    /// at the per-counter level (we don't atomically read the
    /// quartet), but the worst-case skew is one in-flight bump —
    /// negligible for status display.
    pub fn snapshot(&self) -> TokenUsageSnapshot {
        TokenUsageSnapshot {
            calls: self.calls.load(Ordering::Relaxed),
            prompt_tokens: self.prompt_tokens.load(Ordering::Relaxed),
            completion_tokens: self.completion_tokens.load(Ordering::Relaxed),
            total_tokens: self.total_tokens.load(Ordering::Relaxed),
        }
    }
}

/// Plain snapshot of the ledger. Returned by [`DaemonInferenceClient::usage_snapshot`]
/// and serialized to `<workspace>/_tokens.json` by the extract loop
/// so `svrn corpus status` / `/internal/atlas/status` can show
/// per-corpus token spend without re-counting from logs.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, Default)]
pub struct TokenUsageSnapshot {
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl DaemonInferenceClient {
    pub fn new(
        base_url: impl Into<String>,
        chat_model: impl Into<String>,
        embed_model: impl Into<String>,
    ) -> Result<Self> {
        let client = reqwest::Client::builder().timeout(CHAT_TIMEOUT).build()?;
        let base_url_str = base_url.into();
        let providers = Arc::new(ProviderRegistry::load_default(&base_url_str));
        Ok(Self {
            client,
            base_url: base_url_str,
            chat_model: chat_model.into(),
            embed_model: embed_model.into(),
            max_output_tokens: None,
            chat_models_by_phase: BTreeMap::new(),
            max_tokens_by_phase: BTreeMap::new(),
            phase_overrides: BTreeMap::new(),
            usage: Arc::new(TokenUsageLedger::default()),
            providers,
        })
    }

    /// Install per-phase request-shape overrides — temperature,
    /// max_tokens, thinking budget. Each entry, keyed by phase id,
    /// is applied to outgoing prompts whose `phase_id` matches
    /// before the request is dispatched. Empty map is a no-op.
    pub fn with_phase_overrides(
        mut self,
        overrides: BTreeMap<String, super::config::PhaseOverride>,
    ) -> Self {
        self.phase_overrides = overrides;
        self
    }

    /// Shared handle to the underlying token-usage ledger. Survives
    /// `into_closures*` (the closures Arc-wrap the client and bump
    /// the same ledger), so the extract loop holds this handle
    /// before consuming the client and reads cumulative spend via
    /// `ledger.snapshot()`.
    pub fn usage_ledger(&self) -> Arc<TokenUsageLedger> {
        Arc::clone(&self.usage)
    }

    /// Set the per-request output cap. Applies to future `complete`
    /// calls; embed calls are unaffected.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// Install per-phase chat-model overrides. Each `(phase_id,
    /// model_id)` entry in the map takes precedence over the
    /// client's default `chat_model` when a `ChatPrompt` arrives
    /// tagged with that `phase_id`. Phases not in the map (or
    /// untagged prompts) keep the default. An empty map is a no-op.
    ///
    /// Recommended wiring: load `EnrichConfig`, then call
    /// `client.with_chat_models_by_phase(cfg.chat_models_by_phase_snapshot())`
    /// before handing the client off to `into_closures*`.
    pub fn with_chat_models_by_phase(mut self, overrides: BTreeMap<String, String>) -> Self {
        self.chat_models_by_phase = overrides;
        self
    }

    /// Install per-phase max_tokens caps. Phases not in the map fall
    /// through to the client-level `max_output_tokens`. Empty map is
    /// a no-op.
    pub fn with_max_tokens_by_phase(mut self, overrides: BTreeMap<String, u32>) -> Self {
        self.max_tokens_by_phase = overrides;
        self
    }

    /// Resolve the max_tokens cap to apply to a prompt tagged with
    /// `phase_id`. Per-phase override wins over the client-level cap.
    /// Returns `None` when neither is set, meaning "let the daemon
    /// decide".
    fn resolve_max_tokens_for_phase(&self, phase_id: Option<&str>) -> Option<u32> {
        if let Some(id) = phase_id {
            if let Some(n) = self.max_tokens_by_phase.get(id) {
                return Some(*n);
            }
        }
        self.max_output_tokens
    }

    /// Resolve which chat-model id this client will use for a prompt
    /// tagged with `phase_id`. Returns the override if present;
    /// otherwise the default `chat_model`.
    fn resolve_model_for_phase(&self, phase_id: Option<&str>) -> &str {
        if let Some(id) = phase_id {
            if let Some(model) = self.chat_models_by_phase.get(id) {
                return model.as_str();
            }
        }
        self.chat_model.as_str()
    }

    /// Build a client from `EnrichConfig`, threading both the
    /// operator's `max_output_tokens` cap and per-phase chat-model
    /// overrides onto the result. Recommended construction path for
    /// every enrich subcommand — keeps all the per-corpus config
    /// surfaces (timeout, output cap, phase-routing) consistent.
    pub fn from_enrich_config(cfg: &super::config::EnrichConfig) -> Result<Self> {
        let mut max_tokens_by_phase: BTreeMap<String, u32> = BTreeMap::new();
        if let Some(cap) = cfg.phase1b_max_output_tokens {
            // Both Phase 1b coverage variants (entity + concept) share
            // the same shape — schema-free, output-bloated under
            // thinking-disabled models. Apply the cap to both.
            max_tokens_by_phase.insert("phase1b_entity".to_string(), cap);
            max_tokens_by_phase.insert("phase1b_concept".to_string(), cap);
        }
        Ok(Self::new(
            cfg.base_url.clone(),
            cfg.chat_model.clone(),
            cfg.embed_model.clone(),
        )?
        .with_max_output_tokens(cfg.max_output_tokens)
        .with_chat_models_by_phase(cfg.chat_models_by_phase_snapshot())
        .with_max_tokens_by_phase(max_tokens_by_phase)
        .with_phase_overrides(cfg.phase_overrides_snapshot()))
    }

    /// Refine the provider registry's structured-output modes against
    /// live OICP capability manifests before dispatching any request
    /// (see [`ProviderRegistry::discover_structured_output`]). Async,
    /// best-effort, idempotent: an unreachable host leaves every
    /// provider on its configured/default mode. Callers on the enrich
    /// full-run path chain this after [`Self::from_enrich_config`] so
    /// the `local` provider's mode reflects what the daemon actually
    /// advertises (OICP v0.4 §feature-negotiation) rather than a
    /// hard-coded `json_schema`. Skipping it is safe — the default is
    /// correct for a Sovereign daemon.
    pub async fn discover_capabilities(mut self) -> Self {
        let mut registry = (*self.providers).clone();
        registry.discover_structured_output().await;
        self.providers = Arc::new(registry);
        self
    }

    /// Call `/v1/chat/completions` with a single system + user
    /// message. Output-token cap precedence:
    ///   1. `prompt.max_output_tokens` (composer-attached, opts the
    ///      request into OICP-routed FastShort/FastLong selection)
    ///   2. per-phase config (`with_max_tokens_by_phase`)
    ///   3. client default (`with_max_output_tokens`)
    pub async fn complete(&self, prompt: &ChatPrompt) -> Result<String> {
        let cap = prompt
            .max_output_tokens
            .or_else(|| self.resolve_max_tokens_for_phase(prompt.phase_id.as_deref()));
        // Apply per-phase request-shape overrides (temperature /
        // thinking) onto a working copy of the prompt. Composer-
        // attached values on `prompt` win; blank fields inherit from
        // the atlas config's `phase_overrides` map. The dispatcher
        // then layers provider defaults beneath that.
        let prompt_owned = self.apply_phase_overrides(prompt);
        self.complete_inner(&prompt_owned, cap).await
    }

    /// Layer the atlas-config per-phase overrides onto a prompt
    /// without mutating composer-set fields. Composer wins;
    /// otherwise inherit from `phase_overrides[phase_id]`. Returns a
    /// cloned `ChatPrompt` ready to dispatch.
    fn apply_phase_overrides(&self, prompt: &ChatPrompt) -> ChatPrompt {
        let phase = match prompt.phase_id.as_deref() {
            Some(p) => p,
            None => return prompt.clone(),
        };
        let Some(ov) = self.phase_overrides.get(phase) else {
            return prompt.clone();
        };
        let mut out = prompt.clone();
        if out.temperature.is_none() {
            out.temperature = ov.temperature;
        }
        if out.thinking_tokens.is_none() {
            out.thinking_tokens = ov.thinking_tokens;
        }
        // Note: max_tokens override is already handled by the
        // existing `max_tokens_by_phase` path (we leave that as the
        // single source of truth for output-token caps to avoid a
        // second knob with the same effect).
        out
    }

    /// Call `/v1/chat/completions` with a per-call output-token
    /// override. Used by the runner when a retry mode (e.g.
    /// `RetryMode::Terse`) asks for a larger budget on a specific
    /// chapter without mutating the shared client-level cap.
    pub async fn complete_with_tokens(
        &self,
        prompt: &ChatPrompt,
        max_tokens: u32,
    ) -> Result<String> {
        let prompt_owned = self.apply_phase_overrides(prompt);
        self.complete_inner(&prompt_owned, Some(max_tokens)).await
    }

    /// Shared inner path — `complete` and `complete_with_tokens`
    /// differ only in which token cap they pass in. `None` means
    /// "let the daemon decide" (useful for tests and environments
    /// where no cap has been explicitly configured).
    async fn complete_inner(&self, prompt: &ChatPrompt, max_tokens: Option<u32>) -> Result<String> {
        // Parse `provider:model` from the resolved chat-model (or its
        // per-phase override). Bare ids → local provider; explicit
        // provider names dispatch to remote registry entries.
        let raw = self.resolve_model_for_phase(prompt.phase_id.as_deref());
        let (provider_name, model_id) = parse_model_spec(raw);
        let provider = self.providers.get(&provider_name).ok_or_else(|| {
            Error::Serialization(format!(
                "no provider named `{provider_name}` configured (referenced by chat_model `{raw}`); \
                 add a [providers.{provider_name}] block to ~/.config/sovereign/providers.toml \
                 or use a bare model id to fall back to `local`"
            ))
        })?;
        let effective_model = if model_id.is_empty() {
            provider
                .default_model
                .as_deref()
                .ok_or_else(|| Error::Serialization(format!(
                    "model id missing in `{raw}` and provider `{provider_name}` has no default_model"
                )))?
                .to_string()
        } else {
            model_id
        };
        let effective_max_tokens = max_tokens.or(provider.default_max_tokens);
        match provider.kind {
            ProviderKind::OpenaiCompatible => {
                self.complete_openai_compatible(
                    provider,
                    &effective_model,
                    prompt,
                    effective_max_tokens,
                )
                .await
            }
            ProviderKind::Anthropic => {
                self.complete_anthropic(provider, &effective_model, prompt, effective_max_tokens)
                    .await
            }
        }
    }

    /// Original OpenAI-shape `/v1/chat/completions` dispatch. Kept
    /// byte-identical to the pre-multi-provider behavior so the
    /// local daemon path is unchanged.
    async fn complete_openai_compatible(
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
            use super::providers::StructuredOutputMode as M;
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
    async fn complete_anthropic(
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

    /// Call `/v1/embeddings` for a single text. Uses a shorter timeout
    /// than chat since embeds are fast.
    ///
    /// **Per-call timing logged at info** under the
    /// `inference_client: /v1/embeddings ok` event — same shape as the
    /// chat-call telemetry at the top of this module, so a single log
    /// parser (e.g. `scripts/profile-enrich.py`) can aggregate both
    /// surfaces uniformly. Added 2026-05-17 during the SEP-pipeline
    /// profiling pass; the absence of this log line was concealing
    /// how much wall time the per-item embed pattern was costing.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let started = std::time::Instant::now();
        let text_len_chars = text.chars().count();
        let url = format!("{}/v1/embeddings", self.base_url);
        let body = serde_json::json!({
            "model": self.embed_model,
            "input": text,
        });
        // Build a one-shot client with the embed timeout so callers
        // don't share the long chat timeout on what should be <1s.
        let short_client = reqwest::Client::builder().timeout(EMBED_TIMEOUT).build()?;
        let resp = short_client.post(&url).json(&body).send().await?;
        let status = resp.status();
        let payload = resp
            .text()
            .await
            .map_err(|e| Error::Embed(format!("embed read: {e}")))?;
        if !status.is_success() {
            let hint = if status.as_u16() == 404 {
                " (the daemon does not expose an embeddings route — upgrade the daemon \
                 binary or verify it was built with sovereign-mesh's HTTP surface)"
            } else {
                ""
            };
            return Err(Error::Embed(format!(
                "daemon embed error {status} at {url}: {}{}",
                if payload.is_empty() {
                    "<empty body>"
                } else {
                    payload.as_str()
                },
                hint
            )));
        }
        let v: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| Error::Embed(format!("non-JSON embed response: {e}")))?;
        let arr = v
            .pointer("/data/0/embedding")
            .and_then(|x| x.as_array())
            .ok_or_else(|| {
                Error::Embed(format!(
                    "embed response missing data[0].embedding: {payload}"
                ))
            })?;
        let out: Vec<f32> = arr
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
            .collect();
        let elapsed_ms = started.elapsed().as_millis() as u64;
        tracing::info!(
            model = %self.embed_model,
            elapsed_ms,
            text_len_chars,
            embed_dim = out.len(),
            "inference_client: /v1/embeddings ok"
        );
        Ok(out)
    }

    /// Wrap this client as the `(EmbedFn, ChatCompletionFn)` pair that
    /// `PhaseRunner::new` expects.
    pub fn into_closures(self) -> (EmbedFn, ChatCompletionFn) {
        let (embed, chat, _) = self.into_closures_with_tokens();
        (embed, chat)
    }

    /// Wrap this client as an `(EmbedFn, ChatCompletionFn,
    /// ChatCompletionWithTokensFn)` triple. The third closure
    /// routes through `complete_with_tokens`, letting the runner
    /// raise the output budget for specific retries without
    /// rebuilding the HTTP client.
    pub fn into_closures_with_tokens(
        self,
    ) -> (EmbedFn, ChatCompletionFn, ChatCompletionWithTokensFn) {
        let arc = Arc::new(self);
        let embed_arc = arc.clone();
        let embed: EmbedFn = Arc::new(move |text: &str| {
            let this = embed_arc.clone();
            let text = text.to_string();
            Box::pin(async move { this.embed_one(&text).await })
        });
        let chat_arc = arc.clone();
        let chat: ChatCompletionFn = Arc::new(move |prompt: &ChatPrompt| {
            let this = chat_arc.clone();
            let prompt = prompt.clone();
            Box::pin(async move { this.complete(&prompt).await })
        });
        let chat_tokens_arc = arc;
        let chat_with_tokens: ChatCompletionWithTokensFn =
            Arc::new(move |prompt: &ChatPrompt, tokens: u32| {
                let this = chat_tokens_arc.clone();
                let prompt = prompt.clone();
                Box::pin(async move { this.complete_with_tokens(&prompt, tokens).await })
            });
        (embed, chat, chat_with_tokens)
    }
}

/// Readiness probe — returns `true` iff `GET /v1/models` responds
/// 200 within 500ms. Used by `enrich init` / `extract` to fail early
/// if the daemon isn't running.
pub async fn probe_daemon(base_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    let url = if base_url.ends_with("/v1/models") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1/models")
    };
    client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Enumerate the daemon's registered models. Returns `(chat_model, embed_model)`
/// heuristically — the first chat-capable ID and the first embedding ID — or
/// `(None, None)` on any failure.
///
/// The `/v1/models` endpoint doesn't carry capability tags consistently across
/// backends, so we fall back to name-pattern matching: anything containing
/// `"embedding"` or `"-embed"` is classed as embed; everything else is chat.
pub async fn resolve_default_models(base_url: &str) -> (Option<String>, Option<String>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    // The CONFIGURED client port when the caller supplied no base — the
    // compiled default reached the wrong daemon (or the operator's) on any
    // host that moved `client_port`.
    let url = format!(
        "{}/v1/models",
        sovereign_core::setup_config::client_daemon_base()
    );
    // If caller gave us a non-default base, use their URL.
    let url = if base_url.contains("://") && !base_url.ends_with("/v1/models") {
        format!("{}/v1/models", base_url.trim_end_matches('/'))
    } else {
        url
    };
    let Ok(resp) = client.get(&url).send().await else {
        return (None, None);
    };
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return (None, None);
    };
    pick_default_models_from_v1(&v)
}

/// Pure parser over a `/v1/models` payload — split out from the
/// HTTP path so the alias-preference logic is unit-testable.
///
/// Resolves aliases first so the LOCAL primary always wins over
/// mesh-advertised peer models. `/v1/models` aggregates local +
/// peer manifests, so the first non-embed id may be a peer's
/// primary (alphabetical or order-of-arrival) and baking that
/// into a corpus config makes every subsequent `enrich build`
/// request a model this node can't serve.
///
/// The daemon exposes `commonwealth/primary` and
/// `commonwealth/embed` as stable aliases pointing at the
/// local-only models. We pick those first, then walk the rest
/// as a fallback (e.g. a peer-only mesh where the local slot
/// isn't loaded, or an older daemon without the alias surface).
fn pick_default_models_from_v1(v: &serde_json::Value) -> (Option<String>, Option<String>) {
    let Some(arr) = v.get("data").and_then(|d| d.as_array()) else {
        return (None, None);
    };

    let mut chat = None;
    let mut embed = None;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        // Return the alias itself, NOT its resolved GGUF target.
        // Storing `commonwealth/primary` in corpus configs lets
        // the daemon route across the mesh (any peer with a Slow
        // slot loaded can serve `commonwealth/primary`) and makes
        // local model swaps transparent — no per-corpus config
        // rewrites when the underlying GGUF changes.
        if id == "commonwealth/primary" && chat.is_none() {
            chat = Some(id.to_string());
        } else if id == "commonwealth/embed" && embed.is_none() {
            embed = Some(id.to_string());
        }
    }
    if chat.is_none() || embed.is_none() {
        for m in arr {
            let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
                continue;
            };
            if id.starts_with("commonwealth/") {
                continue;
            }
            let lower = id.to_lowercase();
            let is_embed = lower.contains("embedding") || lower.contains("-embed");
            if is_embed {
                if embed.is_none() {
                    embed = Some(id.to_string());
                }
            } else if chat.is_none() {
                chat = Some(id.to_string());
            }
        }
    }
    (chat, embed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_for_phase_falls_back_to_default_without_overrides() {
        let c = DaemonInferenceClient::new("http://localhost:9741", "qwopus-27b", "embed").unwrap();
        assert_eq!(c.resolve_model_for_phase(None), "qwopus-27b");
        assert_eq!(c.resolve_model_for_phase(Some("phase1")), "qwopus-27b");
        assert_eq!(c.resolve_model_for_phase(Some("anything")), "qwopus-27b");
    }

    #[test]
    fn resolve_model_for_phase_routes_via_override_when_phase_id_matches() {
        let mut overrides = BTreeMap::new();
        overrides.insert("phase1".into(), "qwen-9b".into());
        overrides.insert("phase8_configuration".into(), "qwopus-27b".into());
        let c = DaemonInferenceClient::new("http://localhost:9741", "default-model", "embed")
            .unwrap()
            .with_chat_models_by_phase(overrides);
        assert_eq!(c.resolve_model_for_phase(Some("phase1")), "qwen-9b");
        assert_eq!(
            c.resolve_model_for_phase(Some("phase8_configuration")),
            "qwopus-27b"
        );
        // Phase id not in the map → default.
        assert_eq!(c.resolve_model_for_phase(Some("phase5")), "default-model");
        // Untagged prompt → default.
        assert_eq!(c.resolve_model_for_phase(None), "default-model");
    }

    #[tokio::test]
    async fn probe_daemon_returns_false_for_unreachable_host() {
        // Port 1 is reserved and never listening.
        assert!(!probe_daemon("http://127.0.0.1:1").await);
    }

    #[tokio::test]
    async fn resolve_default_models_returns_none_on_unreachable() {
        let (chat, embed) = resolve_default_models("http://127.0.0.1:1").await;
        assert!(chat.is_none());
        assert!(embed.is_none());
    }

    #[test]
    fn pick_default_returns_alias_not_underlying_gguf() {
        // The whole point: enrich corpus configs should store
        // the mesh-stable alias, not the resolved GGUF. Peers
        // advertise `commonwealth/primary` from their own
        // self-manifest (each pointing at its own local Slow
        // slot), so a request for `commonwealth/primary` can
        // route to either node. If we stored the GGUF id here,
        // every model swap on either machine would invalidate
        // every corpus config — that's the brittleness we're
        // fixing.
        let payload = serde_json::json!({
            "data": [
                {"id": "FINAL-Bench_Darwin-36B-Opus-Q6_K", "owned_by": "mesh"},
                {"id": "FINAL-Bench_Darwin-36B-Opus-Q4_K_L", "owned_by": "mesh"},
                {"id": "Qwen3-Embedding-0.6B-Q8_0", "owned_by": "mesh"},
                {"id": "commonwealth/primary", "owned_by": "alias→FINAL-Bench_Darwin-36B-Opus-Q4_K_L"},
                {"id": "commonwealth/embed", "owned_by": "alias→Qwen3-Embedding-0.6B-Q8_0"},
            ]
        });
        let (chat, embed) = pick_default_models_from_v1(&payload);
        assert_eq!(chat.as_deref(), Some("commonwealth/primary"));
        assert_eq!(embed.as_deref(), Some("commonwealth/embed"));
    }

    #[test]
    fn pick_default_falls_back_to_first_non_embed_without_alias() {
        // Older daemon / minimal config: no `commonwealth/*`
        // aliases present. Resolver walks the list and grabs
        // the first non-embed for chat, first embed for embed.
        // The GGUF id is the right answer in this case — the
        // daemon doesn't know about the alias namespace at all.
        let payload = serde_json::json!({
            "data": [
                {"id": "Qwen3-Embedding-0.6B-Q8_0", "owned_by": "mesh"},
                {"id": "Darwin-9B-Opus.Q8_0", "owned_by": "mesh"},
            ]
        });
        let (chat, embed) = pick_default_models_from_v1(&payload);
        assert_eq!(chat.as_deref(), Some("Darwin-9B-Opus.Q8_0"));
        assert_eq!(embed.as_deref(), Some("Qwen3-Embedding-0.6B-Q8_0"));
    }

    #[test]
    fn pick_default_skips_commonwealth_namespace_in_fallback() {
        // Edge case: `commonwealth/fast` is present but
        // `commonwealth/primary` is not (operator misconfig).
        // The fallback should still skip every `commonwealth/*`
        // id rather than picking `commonwealth/fast` as chat —
        // we only know `commonwealth/primary` and `commonwealth/embed`
        // are the canonical chat/embed aliases.
        let payload = serde_json::json!({
            "data": [
                {"id": "commonwealth/fast", "owned_by": "alias→some-fast-model"},
                {"id": "Darwin-36B-Opus", "owned_by": "mesh"},
            ]
        });
        let (chat, _) = pick_default_models_from_v1(&payload);
        assert_eq!(chat.as_deref(), Some("Darwin-36B-Opus"));
    }
}
