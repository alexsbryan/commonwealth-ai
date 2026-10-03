// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP client glue for talking to the Commonwealth daemon's
//! OpenAI-compatible chat + embeddings endpoints.
//!
//! This is the only place in `enrich_cmd/` that knows about
//! reqwest and the wire shape — every other subcommand just
//! takes the pair of closures (`EmbedFn` + `InferenceFn`)
//! produced by `build_client_pair`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::enrichment::pipeline::ChatPrompt;
use corpus_engine::error::{Error, Result};
use corpus_engine::types::EmbedFn;
use corpus_engine::InferenceFn;

use oicp_client::StructuredOutputMode;
use oicp_types::ShardingPrivacy;
use sovereign_contracts::egress::{model_client, release, verify, ConsentGrant, EgressPayload};
use sovereign_contracts::types::{Custody, SearchPrivacy};

mod consent;
mod negotiate;
mod wire;

pub use consent::{export_run_consent, parse_consent_class, run_consent};

// The two `/v1/models` probes are free functions every caller runs BEFORE it
// has a client, so they surface here rather than as methods. They live in the
// corpus-index leaf (`corpus_index::v1_models`, pb-code-clean) so a program
// that does not link this crate reaches the same decider. `wire` holds the
// chat dispatch as a second `impl DaemonInferenceClient` block and exports
// nothing: its methods are `pub(super)`, reachable from this module and no
// further.
pub use corpus_index::v1_models::{probe_daemon, resolve_default_models};

// The three-way probe (order enrich-probe-timeout) stays INSIDE the
// crate: `probe_daemon`'s bool signature is frozen by the ~20
// `sovereign-cli-llm` call sites that reach this module through the
// re-export there, so the slow-vs-down split surfaces only to this
// crate's own callers (extract) — it must not escape the module.
pub(crate) use corpus_index::v1_models::{probe_daemon_status, DaemonProbe, V1_MODELS_TIMEOUT};

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

/// Reusable OpenAI-compatible chat client pointed at one host: this
/// machine's daemon, or a bare llama-server / Ollama / vLLM endpoint. A
/// hosted model is the daemon's business (its `[engine]`), not this client's.
#[derive(Debug, Clone)]
pub struct DaemonInferenceClient {
    client: reqwest::Client,
    /// The chat host's `/v1` base.
    chat_base: String,
    /// The chat host is on this machine (`oicp_client::endpoint_is_loopback`).
    /// Anything else is a remote payload, gated by the run's grant.
    on_box: bool,
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
    /// the dispatcher's defaults.
    phase_overrides: BTreeMap<String, super::config::PhaseOverride>,
    /// Phase D2 — token ledger. Atomic counters bumped on every
    /// successful `complete_inner` call. Cloned across `Clone`d
    /// clients (Arc-wrapped), so the extract loop sees a unified
    /// total even when `EmbedFn` / `InferenceFn` closures are
    /// constructed from a clone (`build_client_pair` does this).
    /// Cheap (relaxed atomics; no lock) so the hot path stays hot.
    usage: Arc<TokenUsageLedger>,
    /// How the chat host is asked for schema-shaped output: `json_schema`
    /// until [`Self::discover_capabilities`] reads what the host advertises.
    structured_output_mode: StructuredOutputMode,
    /// The egress boundary (order deep-research-t2a): the custody class of
    /// every payload this client sends. `Personal`: an extraction chunk is
    /// the estate's own content.
    payload_custody: Custody,
    /// The run's consent grant (`--consent`). `None` is default-deny: a
    /// payload bound for another host refuses, naming what was withheld.
    consent: Option<ConsentGrant>,
    /// How far the run's grant lets a payload travel, declared on every
    /// request's envelope: `ThirdPartyAllowed` when `egress::release` frees
    /// this client's custody, which is what lets a daemon whose engine is a
    /// hosted vendor serve it. `None` declares nothing. Fixed with the grant.
    reach: Option<ShardingPrivacy>,
    /// Where `POST /v1/embeddings` goes, which is `base_url` for every
    /// host that serves both models — a daemon, Ollama, vLLM — and a
    /// DIFFERENT process for `llama-server`, which serves one model per
    /// process (order ei-5b-build-verb). Set from
    /// `EnrichConfig::embed_base`, so a corpus configured with two
    /// endpoints reaches both from every phase rather than sending its
    /// resolution embeddings to the chat process, which answers 200 with
    /// a vector of the wrong width.
    embed_base_url: String,
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
        let chat_base = v1_base(&base_url_str);
        Ok(Self {
            client,
            on_box: oicp_client::endpoint_is_loopback(&chat_base),
            chat_base,
            chat_model: chat_model.into(),
            embed_model: embed_model.into(),
            max_output_tokens: None,
            chat_models_by_phase: BTreeMap::new(),
            max_tokens_by_phase: BTreeMap::new(),
            phase_overrides: BTreeMap::new(),
            usage: Arc::new(TokenUsageLedger::default()),
            structured_output_mode: StructuredOutputMode::default(),
            payload_custody: Custody::Personal,
            consent: None,
            reach: None,
            // One host until told otherwise. `with_embed_base_url` is how a
            // caller says the embeddings live somewhere else.
            embed_base_url: base_url_str,
        })
    }

    /// Point `POST /v1/embeddings` at a different host from the chat one.
    ///
    /// The case this exists for is `llama-server`, which loads one model per
    /// process: chat on :8090 and embeddings on :8089 are two servers, not
    /// two routes. Callers that hold one URL for both never call this.
    pub fn with_embed_base_url(mut self, base: impl Into<String>) -> Self {
        self.embed_base_url = base.into();
        self
    }

    /// Install the run-scoped consent grant consulted at the egress
    /// boundary when the chat host is off this machine. `None` (the
    /// default) is default-deny. A grant that releases this client's custody
    /// also sets the run's reach, which every request declares.
    pub fn with_consent(mut self, consent: Option<ConsentGrant>) -> Self {
        self.consent = consent;
        let released = release(self.payload_custody, self.consent.as_ref());
        self.reach = released.ok().map(|_| ShardingPrivacy::ThirdPartyAllowed);
        tracing::info!(
            target: "egress",
            custody = %self.payload_custody,
            ?released,
            reach = ?self.reach,
            "enrich: the run's reach, declared on every request's envelope"
        );
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
        .with_embed_base_url(cfg.embed_base())
        .with_max_output_tokens(cfg.max_output_tokens)
        .with_chat_models_by_phase(cfg.chat_models_by_phase_snapshot())
        .with_max_tokens_by_phase(max_tokens_by_phase)
        .with_phase_overrides(cfg.phase_overrides_snapshot())
        .with_consent(run_consent(&cfg.corpus_id)))
    }

    /// Refine the structured-output mode against the chat host's live OICP
    /// capability manifest before dispatching any request
    /// (`negotiate::discover_structured_output`). Best-effort: an
    /// unreachable or non-OICP host keeps `json_schema`. Callers on the
    /// enrich full-run path chain this after [`Self::from_enrich_config`].
    /// Skipping it is safe — the default is correct for a Sovereign daemon.
    pub async fn discover_capabilities(mut self) -> Self {
        self.structured_output_mode =
            negotiate::discover_structured_output(&self.chat_base, self.structured_output_mode)
                .await;
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
        // The model id goes to the host verbatim: which backend serves it is
        // the host's business, and an id may carry a colon (`qwen3:8b`).
        let model = self.resolve_model_for_phase(prompt.phase_id.as_deref());
        // The egress boundary (order deep-research-t2a, R-5): every dispatch
        // passes the ONE release gate before any request is built. A host on
        // this machine is Local; any other is External, and a personal chunk
        // refuses there without the run's grant.
        let privacy = if self.on_box {
            SearchPrivacy::Local
        } else {
            SearchPrivacy::External {
                provider: "openai-compatible",
            }
        };
        verify(
            &EgressPayload {
                privacy,
                custody: self.payload_custody,
                what: "chunk",
                target: &self.chat_base,
                detail: &prompt.user,
                user_formed: false,
            },
            self.consent.as_ref(),
        )
        .map_err(|r| {
            Error::Safety(format!(
                "{r}. To release this corpus's text to {}, pass --consent <class> \
                 (public-web | peer | personal) or set SVRNMESH_EGRESS_CONSENT",
                self.chat_base
            ))
        })?;
        self.complete_openai_compatible(model, prompt, max_tokens)
            .await
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
        let url = format!("{}/v1/embeddings", self.embed_base_url);
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

    /// Wrap this client as the `(EmbedFn, InferenceFn)` pair that
    /// `PhaseRunner::new` expects. The one chat closure carries the
    /// per-call `max_tokens` override in its second argument — `Some(n)`
    /// routes through `complete_with_tokens`, `None` through `complete`
    /// — so the runner's retry paths call the same closure the default
    /// path does (the `into_closures_with_tokens` triple collapsed onto
    /// this pair 2026-09-17, ARCH 8).
    pub fn into_closures(self) -> (EmbedFn, InferenceFn) {
        let arc = Arc::new(self);
        let embed_arc = arc.clone();
        let embed: EmbedFn = Arc::new(move |text: &str| {
            let this = embed_arc.clone();
            let text = text.to_string();
            Box::pin(async move { this.embed_one(&text).await })
        });
        let chat_arc = arc;
        let chat: InferenceFn = Arc::new(move |prompt: &ChatPrompt, max_tokens: Option<u32>| {
            let this = chat_arc.clone();
            let prompt = prompt.clone();
            Box::pin(async move {
                match max_tokens {
                    Some(tokens) => this.complete_with_tokens(&prompt, tokens).await,
                    None => this.complete(&prompt).await,
                }
            })
        });
        (embed, chat)
    }
}

/// The host's `/v1` base. Atlas configs carry the bare host
/// (`http://localhost:9741`), so `/v1` is appended unless the base already
/// names a `/vN` segment.
fn v1_base(raw: &str) -> String {
    if raw.contains("/v1") || raw.contains("/v2") || raw.contains("/v3") {
        raw.to_string()
    } else {
        format!("{}/v1", raw.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Atlas configs carry the bare host; the chat route must land on `/v1`
    /// once, not twice.
    #[test]
    fn the_chat_base_gets_v1_once() {
        assert_eq!(v1_base("http://localhost:9741"), "http://localhost:9741/v1");
        assert_eq!(
            v1_base("http://localhost:9741/"),
            "http://localhost:9741/v1"
        );
        assert_eq!(
            v1_base("http://localhost:9741/v1"),
            "http://localhost:9741/v1"
        );
    }

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
