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

use sovereign_contracts::types::{Custody, SearchPrivacy};
use sovereign_core::egress::{model_client, verify, ConsentGrant, EgressPayload};

use super::providers::{
    local_daemon_base, parse_model_spec, ProviderKind, ProviderRegistry, ResolvedProvider,
};

mod discovery;
mod wire;

// `discovery`'s two probes are free functions every caller runs BEFORE it
// has a client, so they surface here rather than as methods. `wire` holds the
// two provider dialects as a second `impl DaemonInferenceClient` block and
// exports nothing: its methods are `pub(super)`, reachable from this module
// and no further.
pub use discovery::{probe_daemon, resolve_default_models};

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
    /// The egress boundary (order deep-research-t2a): the custody
    /// class this client declares for payloads it would send to a
    /// REMOTE provider. Default `Personal` — an enrich extraction
    /// chunk is the estate's own content and must never leave the
    /// machine without a consent grant. Local-daemon dispatch (the
    /// built-in `local` provider) is never gated — that traffic
    /// never leaves the machine.
    payload_custody: Custody,
    /// Run-scoped consent grant, consulted at the boundary when a
    /// remote provider is resolved. Default `None` — default-deny:
    /// a remote-provider dispatch refuses with a typed message
    /// naming what was withheld (the R-5 red, green). The t2b seat
    /// lands the grant surface for enrich; until then the refusal is
    /// the product behavior.
    consent: Option<ConsentGrant>,
    /// The derived base URL of THIS client's own daemon (the same
    /// normalization the built-in `local` entry gets — see
    /// `providers::local_daemon_base`). The gate compares a resolved
    /// provider's `base_url` against this to tell local-daemon
    /// dispatch from a remote payload.
    local_base: String,
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
        // The ONE egress boundary (order deep-research-t2a): the
        // chat client is built by sovereign-core's egress module (the
        // F26 census counts this file's remaining sites LocalDaemon),
        // with enrich's documented 1800s hang headroom passed in.
        let client = model_client(CHAT_TIMEOUT)?;
        let base_url_str = base_url.into();
        let providers = Arc::new(ProviderRegistry::load_default(&base_url_str));
        Ok(Self {
            client,
            base_url: base_url_str.clone(),
            chat_model: chat_model.into(),
            embed_model: embed_model.into(),
            max_output_tokens: None,
            chat_models_by_phase: BTreeMap::new(),
            max_tokens_by_phase: BTreeMap::new(),
            phase_overrides: BTreeMap::new(),
            usage: Arc::new(TokenUsageLedger::default()),
            providers,
            // Default-deny at t2a: a personal-corpus chunk to a
            // remote provider refuses unless a consent grant covers
            // it (the R-5 red → green). The t2b seat lands the grant
            // surface for enrich.
            payload_custody: Custody::Personal,
            consent: None,
            local_base: local_daemon_base(&base_url_str),
        })
    }

    /// Install the run-scoped consent grant consulted at the egress
    /// boundary when a remote provider is resolved. `None` (the
    /// default) is default-deny.
    pub fn with_consent(mut self, consent: Option<ConsentGrant>) -> Self {
        self.consent = consent;
        self
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
        // The egress boundary (order deep-research-t2a, R-5): a
        // resolved provider whose endpoint is NOT this client's own
        // daemon is a remote payload — it passes the ONE release gate
        // BEFORE any request is built. Default custody Personal + no
        // grant → typed refusal naming what was withheld. Dispatch to
        // the built-in local provider (endpoint == this daemon) never
        // leaves the machine and skips the gate.
        if provider.base_url != self.local_base {
            verify(
                &EgressPayload {
                    privacy: SearchPrivacy::External {
                        provider: match provider.kind {
                            ProviderKind::Anthropic => "anthropic",
                            ProviderKind::OpenaiCompatible => "openai-compatible",
                        },
                    },
                    custody: self.payload_custody,
                    what: "chunk",
                    target: &provider.name,
                    detail: &prompt.user,
                    user_formed: false,
                },
                self.consent.as_ref(),
            )
            .map_err(|r| Error::Safety(format!("{r}")))?;
        }
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
        // The embed route sheds under load like the chat route does, and a
        // backfill drops an atom's vector when it treats that as an error.
        let (status, payload) = wire::send_honouring_shed(
            &short_client.post(&url).json(&body),
            "daemon embed",
            "embed",
        )
        .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the client's own phase->model resolution, which is
    // private to this module. They arrived in `discovery.rs` with the split
    // and came back: a test belongs with the thing it tests, and widening
    // `resolve_model_for_phase` to satisfy a misplaced test would have put a
    // private detail on the module's surface.
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
}
