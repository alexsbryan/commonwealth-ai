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

use crate::util::urls::{v1_models_url, DEFAULT_CLIENT_PORT};

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
    /// Phase D2 — token ledger. Atomic counters bumped on every
    /// successful `complete_inner` call. Cloned across `Clone`d
    /// clients (Arc-wrapped), so the extract loop sees a unified
    /// total even when `EmbedFn` / `ChatCompletionFn` closures are
    /// constructed from a clone (`build_client_pair` does this).
    /// Cheap (relaxed atomics; no lock) so the hot path stays hot.
    usage: Arc<TokenUsageLedger>,
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
/// so `sovereign corpus status` / `/internal/atlas/status` can show
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
        let client = reqwest::Client::builder()
            .timeout(CHAT_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            chat_model: chat_model.into(),
            embed_model: embed_model.into(),
            max_output_tokens: None,
            chat_models_by_phase: BTreeMap::new(),
            max_tokens_by_phase: BTreeMap::new(),
            usage: Arc::new(TokenUsageLedger::default()),
        })
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
    pub fn with_chat_models_by_phase(
        mut self,
        overrides: BTreeMap<String, String>,
    ) -> Self {
        self.chat_models_by_phase = overrides;
        self
    }

    /// Install per-phase max_tokens caps. Phases not in the map fall
    /// through to the client-level `max_output_tokens`. Empty map is
    /// a no-op.
    pub fn with_max_tokens_by_phase(
        mut self,
        overrides: BTreeMap<String, u32>,
    ) -> Self {
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
        .with_max_tokens_by_phase(max_tokens_by_phase))
    }

    /// Call `/v1/chat/completions` with a single system + user
    /// message. Uses the per-phase cap if `prompt.phase_id` matches
    /// one configured via `with_max_tokens_by_phase`, otherwise the
    /// client-level `max_output_tokens` configured via
    /// `with_max_output_tokens`.
    pub async fn complete(&self, prompt: &ChatPrompt) -> Result<String> {
        let cap = self.resolve_max_tokens_for_phase(prompt.phase_id.as_deref());
        self.complete_inner(prompt, cap).await
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
        self.complete_inner(prompt, Some(max_tokens)).await
    }

    /// Shared inner path — `complete` and `complete_with_tokens`
    /// differ only in which token cap they pass in. `None` means
    /// "let the daemon decide" (useful for tests and environments
    /// where no cap has been explicitly configured).
    async fn complete_inner(
        &self,
        prompt: &ChatPrompt,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let model = self.resolve_model_for_phase(prompt.phase_id.as_deref());
        // `think_budget: 0` instructs the daemon to inject `/no_think`
        // for SystemPromptToken thinking families (Qwen3 / Qwen3.5 /
        // SmolLM3). The schema constraint already forces JSON
        // correctness for atlas Phase 1; chain-of-thought tokens are
        // pure latency cost — Qwen3.5-4B with thinking disabled went
        // from 60+ s/chapter to ~10 s/chapter on the wiki-tier2-bank
        // run. Models without SystemPromptToken thinking control
        // (Gemma 3/4, Llama 3, Phi-4) ignore this field harmlessly.
        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": prompt.system},
                {"role": "user", "content": prompt.user},
            ],
            "temperature": 0.2,
            "stream": false,
            "think_budget": 0,
        });
        if let Some(n) = max_tokens {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("max_tokens".into(), serde_json::json!(n));
            }
        }
        // OpenAI-style structured-output declaration. When the
        // pipeline attached a JSON Schema via
        // `ChatPrompt::with_response_schema`, surface it as
        // `response_format` so the daemon installs a
        // grammar-constrained sampler. Schema name defaults to a
        // generic label if the caller didn't set one.
        let mut has_schema = false;
        if let Some(schema) = prompt.response_schema.as_ref() {
            let name = prompt
                .response_schema_name
                .as_deref()
                .unwrap_or("response_schema");
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "response_format".into(),
                    serde_json::json!({
                        "type": "json_schema",
                        "json_schema": {
                            "name": name,
                            "schema": schema,
                            "strict": true
                        }
                    }),
                );
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

        let result = self.client.post(&url).json(&body).send().await;
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
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| Error::Serialization(format!("non-JSON chat response: {e} — body: {text}")))?;
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
        tracing::info!(
            phase = %phase_label,
            model = %model_label,
            elapsed_ms = elapsed_ms as u64,
            total_tokens,
            completion_tokens,
            tok_per_s = format!("{tok_per_s:.1}"),
            "inference_client: /v1/chat/completions ok"
        );
        Ok(content.to_string())
    }

    /// Call `/v1/embeddings` for a single text. Uses a shorter timeout
    /// than chat since embeds are fast.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/v1/embeddings", self.base_url);
        let body = serde_json::json!({
            "model": self.embed_model,
            "input": text,
        });
        // Build a one-shot client with the embed timeout so callers
        // don't share the long chat timeout on what should be <1s.
        let short_client = reqwest::Client::builder()
            .timeout(EMBED_TIMEOUT)
            .build()?;
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
                if payload.is_empty() { "<empty body>" } else { payload.as_str() },
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
        Ok(arr
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
            .collect())
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
    let url = v1_models_url(DEFAULT_CLIENT_PORT);
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
    let Some(arr) = v.get("data").and_then(|d| d.as_array()) else {
        return (None, None);
    };

    let mut chat = None;
    let mut embed = None;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
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
    (chat, embed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_for_phase_falls_back_to_default_without_overrides() {
        let c =
            DaemonInferenceClient::new("http://localhost:9741", "qwopus-27b", "embed").unwrap();
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
}
