// SPDX-License-Identifier: AGPL-3.0-or-later
//! Multi-provider routing for `enrich extract` chat calls.
//!
//! The default flow is local-only: requests go to the running
//! Sovereign daemon at `localhost:9741` via OpenAI-compatible
//! `/v1/chat/completions`. Operators who want to offload Phase 1
//! extraction (which dominates ingest cost) to a remote API
//! configure provider entries in `~/.config/sovereign/providers.toml`
//! and reference them in the per-atlas `chat_model` field via a
//! `provider:model_id` syntax (e.g. `anthropic:claude-sonnet-4-5`).
//!
//! Bare model ids without the `provider:` prefix resolve to the
//! built-in `local` provider — preserving backwards compatibility
//! for every existing atlas config.
//!
//! ## Provider types
//!
//! - `openai-compatible` — `/v1/chat/completions` shape, optional bearer
//!   auth. Works for the local daemon, OpenAI, OpenRouter, vLLM,
//!   anything OpenAI-shaped.
//! - `anthropic` — `/v1/messages` shape with `x-api-key` auth, native
//!   tool-use for structured output, optional extended-thinking budget.
//!
//! ## Per-phase + per-call knobs
//!
//! Each provider entry can declare default knobs (model, temperature,
//! max_tokens, thinking_budget_tokens). Per-atlas `phase_overrides`
//! refine these per-phase. The dispatcher merges:
//!   1. per-call (composer-attached `ChatPrompt::max_output_tokens`)
//!   2. per-phase atlas override
//!   3. provider default
//!   4. dispatcher hard default

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Top-level config file shape: `providers.<name> = ProviderConfig`.
/// Loaded from `~/.config/sovereign/providers.toml`. Missing file is
/// treated as "no remote providers configured" — `local` is always
/// synthesized.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

/// One provider entry. `kind` selects the dialect; the rest is its
/// connection + auth + default knobs. Auth secrets are referenced by
/// env var name, not embedded in the config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// `openai-compatible` | `anthropic`. Selects the wire dialect
    /// the dispatcher uses to talk to `base_url`.
    #[serde(rename = "type")]
    pub kind: ProviderKind,
    /// API base, INCLUDING the version segment. The dispatcher
    /// appends only the dialect-specific endpoint suffix
    /// (`/chat/completions` for openai-compatible, `/messages` for
    /// anthropic). Examples:
    ///   - `http://localhost:9741/v1`
    ///   - `https://api.openai.com/v1`
    ///   - `https://api.deepseek.com/v1`
    ///   - `https://api.deepseek.com/anthropic/v1`
    ///   - `https://api.anthropic.com/v1`
    /// Bare hosts (no `/v1`) are accepted for backwards compat —
    /// the local-provider fallback auto-appends `/v1` when synthesizing.
    pub base_url: String,
    /// Env var name holding the bearer / api key. Empty / unset =
    /// fall through to `api_key` (literal) or no auth.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Literal api key. Use this OR `api_key_env`; if both are set,
    /// `api_key_env` wins (the env var indirection is the more
    /// security-conscious path). Convenient for personal setups
    /// where the providers.toml is already gitignored / private.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Default model id used when the caller doesn't specify one
    /// after `provider:`. Optional; the `provider:` syntax requires
    /// a model id explicitly, so this is a fallback for setups that
    /// pin a default per provider.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Default request temperature. Falls through to dispatcher
    /// default (0.2) when unset.
    #[serde(default)]
    pub default_temperature: Option<f32>,
    /// Default max output tokens. Per-call / per-phase overrides win.
    #[serde(default)]
    pub default_max_tokens: Option<u32>,
    /// Anthropic extended-thinking budget tokens (or OpenAI o1
    /// `reasoning_effort`-mapped equivalent). Ignored by providers
    /// that don't support thinking. `Some(0)` disables thinking
    /// (skip the `<think>` block); `None` leaves the provider's
    /// default. Per-phase overrides win.
    #[serde(default)]
    pub default_thinking_tokens: Option<u32>,
    /// Anthropic `anthropic-version` header value. Ignored by
    /// non-anthropic providers. Defaults to `2023-06-01` if unset.
    #[serde(default)]
    pub api_version: Option<String>,
    /// How to ask for structured output. Different providers
    /// implement schema enforcement differently:
    ///
    /// - `json-schema` (default for openai-compatible) — sends
    ///   `response_format: {type: "json_schema", json_schema: {...}}`.
    ///   The provider grammar-constrains the output. Works with the
    ///   local daemon, OpenAI, and OICP-aware OpenAI-compat servers.
    ///   Fails on DeepSeek which only honors `json_object`.
    /// - `json-object` — sends `response_format: {type: "json_object"}`.
    ///   No schema enforcement; relies on prompt + post-validation.
    ///   Required by DeepSeek's chat-completions API.
    /// - `tool-use-auto` (default for anthropic) — exposes the schema
    ///   as a tool with `tool_choice: "auto"`. Model voluntarily
    ///   calls the tool. Works on Anthropic native and DeepSeek's
    ///   Anthropic-compat layer (forced tool_choice fails on
    ///   DeepSeek-reasoner).
    /// - `tool-use-forced` — forces the model to call the tool via
    ///   `tool_choice: {type: "tool", name: ...}`. Maximum schema
    ///   adherence. Works on Anthropic native; rejected by DeepSeek.
    #[serde(default)]
    pub structured_output_mode: Option<StructuredOutputMode>,
    /// Extra request-level params passed through verbatim. Useful
    /// for vendor-specific knobs we don't model explicitly (e.g.
    /// OpenAI `seed`, Anthropic `top_k`). Merged into the request
    /// body after the dispatcher's normal field set.
    #[serde(default)]
    pub extra_params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum ProviderKind {
    /// `/v1/chat/completions` JSON shape. Local daemon speaks this;
    /// so does OpenAI, OpenRouter, Together, vLLM.
    #[default]
    OpenaiCompatible,
    /// `/v1/messages` JSON shape (Anthropic). Different request body
    /// layout (`system` is top-level, `tools` for structured output).
    Anthropic,
}

/// How structured output (JSON Schema) is communicated to the
/// provider. See `ProviderConfig::structured_output_mode` for
/// per-provider semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StructuredOutputMode {
    /// `response_format: {type: "json_schema", json_schema: {...}}`.
    /// Provider enforces the schema (OpenAI, local daemon).
    JsonSchema,
    /// `response_format: {type: "json_object"}` — provider guarantees
    /// valid JSON but no schema enforcement (DeepSeek).
    JsonObject,
    /// `tools=[{schema}], tool_choice: "auto"` — Anthropic-shape;
    /// model voluntarily calls. Works on DeepSeek's Anthropic layer.
    ToolUseAuto,
    /// `tools=[{schema}], tool_choice: {type: "tool", name: ...}` —
    /// Anthropic-shape with forced tool_choice. Maximum adherence;
    /// not all models support it.
    ToolUseForced,
}

/// Parse a model spec of the form `provider:model_id` or bare
/// `model_id`. Bare resolves to provider=`local`. Empty model id is
/// allowed when the resolved provider has a `default_model` set.
pub fn parse_model_spec(spec: &str) -> (String, String) {
    if let Some((provider, model)) = spec.split_once(':') {
        // Heuristic: an empty provider half (e.g. `:foo`) is treated
        // as a bare model id, not a malformed spec, so callers can
        // still pass quirky model ids that happen to contain `:`.
        if !provider.is_empty() {
            return (provider.to_string(), model.to_string());
        }
    }
    ("local".to_string(), spec.to_string())
}

/// Resolved + materialized provider state — kind, base_url, ready
/// auth header, defaults applied. Built by the registry once at
/// load time so the hot path doesn't re-read config files.
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    /// Pre-resolved auth header value (e.g. `Bearer sk-...` or
    /// `<api-key>`). `None` means no auth header is sent. We hold
    /// the value-only here; the dispatcher knows the header NAME
    /// from `kind` (Bearer for OpenAI, `x-api-key` for Anthropic).
    pub auth_secret: Option<String>,
    pub default_model: Option<String>,
    pub default_temperature: Option<f32>,
    pub default_max_tokens: Option<u32>,
    pub default_thinking_tokens: Option<u32>,
    pub api_version: Option<String>,
    pub extra_params: Option<serde_json::Value>,
    pub structured_output_mode: StructuredOutputMode,
    /// True when the operator set `structured_output_mode` explicitly
    /// in `providers.toml`. Capability discovery
    /// ([`ProviderRegistry::discover_structured_output`]) refines only
    /// providers where this is `false` — an explicit config is a hard
    /// override the host's advertised features never overturn.
    pub structured_output_configured: bool,
}

/// Registry of every configured provider plus the synthesized
/// built-in `local` entry. Cheap to clone (Arcs internally would be
/// nice; for now we hold it by reference at the call site).
#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, ResolvedProvider>,
}

impl ProviderRegistry {
    /// Load `~/.config/sovereign/providers.toml` if present, merge
    /// the built-in `local` provider, resolve env-var auth, and
    /// return a ready-to-dispatch registry. Missing config file is
    /// not an error — operators who only run locally never need to
    /// create one.
    pub fn load_default(local_base_url: &str) -> Self {
        let config_path = std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".config/sovereign/providers.toml"));
        let mut providers: HashMap<String, ResolvedProvider> = HashMap::new();

        // Load config file (if any).
        if let Some(path) = config_path.as_ref() {
            if let Ok(text) = std::fs::read_to_string(path) {
                match toml::from_str::<ProvidersConfig>(&text) {
                    Ok(parsed) => {
                        for (name, cfg) in parsed.providers {
                            // Auth resolution: `api_key_env` is an
                            // env var NAME (indirection — best
                            // practice for shared / version-controlled
                            // configs). `api_key` is a literal value
                            // (convenience for private setups). If
                            // both are set, env wins. If neither
                            // resolves to a non-empty string, the
                            // provider runs without auth — fine for
                            // the local daemon; will 401 at remotes.
                            let auth_secret = cfg
                                .api_key_env
                                .as_ref()
                                .and_then(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
                                .or_else(|| {
                                    cfg.api_key.as_ref().filter(|s| !s.is_empty()).cloned()
                                });
                            providers.insert(
                                name.clone(),
                                ResolvedProvider {
                                    name: name.clone(),
                                    kind: cfg.kind.clone(),
                                    base_url: cfg.base_url,
                                    auth_secret,
                                    default_model: cfg.default_model,
                                    default_temperature: cfg.default_temperature,
                                    default_max_tokens: cfg.default_max_tokens,
                                    default_thinking_tokens: cfg.default_thinking_tokens,
                                    api_version: cfg.api_version,
                                    extra_params: cfg.extra_params,
                                    structured_output_configured: cfg
                                        .structured_output_mode
                                        .is_some(),
                                    structured_output_mode: cfg.structured_output_mode.unwrap_or(
                                        match cfg.kind {
                                            ProviderKind::OpenaiCompatible => {
                                                StructuredOutputMode::JsonSchema
                                            }
                                            ProviderKind::Anthropic => {
                                                StructuredOutputMode::ToolUseAuto
                                            }
                                        },
                                    ),
                                },
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: failed to parse {}: {e} — ignoring (using built-in local provider only)",
                            path.display()
                        );
                    }
                }
            }
        }

        // Always synthesize the built-in `local` provider, unless
        // the operator explicitly redefined it. Idempotent: if
        // `local` is in the config file, theirs wins.
        // Backwards compat: existing atlas configs pass the bare
        // host `http://localhost:9741` as `base_url`. Auto-append
        // `/v1` when the base doesn't already carry a /vN/ segment,
        // so the dispatcher's `{base_url}/chat/completions`
        // construction lands at `/v1/chat/completions`.
        let local_base = if local_base_url.contains("/v1")
            || local_base_url.contains("/v2")
            || local_base_url.contains("/v3")
        {
            local_base_url.to_string()
        } else {
            format!("{}/v1", local_base_url.trim_end_matches('/'))
        };
        providers
            .entry("local".to_string())
            .or_insert_with(|| ResolvedProvider {
                name: "local".to_string(),
                kind: ProviderKind::OpenaiCompatible,
                base_url: local_base,
                auth_secret: None,
                default_model: None,
                default_temperature: Some(0.2),
                default_max_tokens: None,
                default_thinking_tokens: Some(0),
                api_version: None,
                extra_params: None,
                // The synthesized `local` provider carries the
                // OpenAI-compat default; discovery may refine it
                // against the daemon's advertised OICP features.
                structured_output_configured: false,
                structured_output_mode: StructuredOutputMode::JsonSchema,
            });

        Self { providers }
    }

    pub fn get(&self, name: &str) -> Option<&ResolvedProvider> {
        self.providers.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// Refine each OpenAI-compatible provider's structured-output mode
    /// against the host's live OICP capability manifest, for providers
    /// where the operator did not pin the mode in `providers.toml`.
    ///
    /// OICP v0.4 §feature-negotiation is normative: a client MUST NOT
    /// send a constraint the host does not advertise. So rather than
    /// assume `json_schema` (the OpenAI-compat default), we read the
    /// host's advertised `features` and pick the strongest constraint
    /// it actually supports (`constraint:json_schema` →
    /// [`StructuredOutputMode::JsonSchema`], else
    /// `constraint:json_object` → [`StructuredOutputMode::JsonObject`]).
    ///
    /// Best-effort and non-fatal: a host that doesn't answer
    /// `/oicp/v1/capabilities` (a v0.3 host, a non-OICP OpenAI-compat
    /// server, or an offline daemon) leaves the provider's
    /// configured/default mode untouched. Anthropic providers are
    /// skipped (their structured output rides `tools`, not
    /// `response_format`). Explicitly-configured providers are never
    /// overridden. Partial adoption is safe — the mode is a
    /// per-request choice, so a caller that skips discovery simply
    /// keeps the sensible default.
    pub async fn discover_structured_output(&mut self) {
        for prov in self.providers.values_mut() {
            if prov.structured_output_configured
                || prov.kind != ProviderKind::OpenaiCompatible
            {
                continue;
            }
            let manifest = match sovereign_inference::remote::fetch_manifest(
                &prov.base_url,
                prov.auth_secret.clone(),
            )
            .await
            {
                Some(m) => m,
                None => {
                    tracing::debug!(
                        provider = %prov.name,
                        base = %prov.base_url,
                        mode = ?prov.structured_output_mode,
                        "OICP manifest unavailable — keeping default structured-output mode"
                    );
                    continue;
                }
            };
            match derive_structured_mode_from_features(&manifest.features) {
                Some(mode) if mode != prov.structured_output_mode => {
                    tracing::info!(
                        provider = %prov.name,
                        from = ?prov.structured_output_mode,
                        to = ?mode,
                        "structured-output mode set from advertised OICP features"
                    );
                    prov.structured_output_mode = mode;
                }
                Some(mode) => {
                    tracing::debug!(
                        provider = %prov.name,
                        mode = ?mode,
                        "advertised OICP features confirm structured-output mode"
                    );
                }
                None => {
                    tracing::debug!(
                        provider = %prov.name,
                        mode = ?prov.structured_output_mode,
                        "OICP features advertise no constraint:* — keeping default structured-output mode"
                    );
                }
            }
        }
    }
}

/// Pick the structured-output mode a host's advertised OICP `features`
/// support: `constraint:json_schema` → [`StructuredOutputMode::JsonSchema`]
/// (schema-enforced, the strongest), else `constraint:json_object` →
/// [`StructuredOutputMode::JsonObject`] (valid JSON, no schema). `None`
/// when the features name neither — the caller keeps its default rather
/// than downgrade blindly. `lark` is a distinct capability, not on this
/// ladder.
pub fn derive_structured_mode_from_features(features: &[String]) -> Option<StructuredOutputMode> {
    use oicp_types::features::{CONSTRAINT_JSON_OBJECT, CONSTRAINT_JSON_SCHEMA};
    if features.iter().any(|f| f == CONSTRAINT_JSON_SCHEMA) {
        Some(StructuredOutputMode::JsonSchema)
    } else if features.iter().any(|f| f == CONSTRAINT_JSON_OBJECT) {
        Some(StructuredOutputMode::JsonObject)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_model_resolves_local() {
        let (p, m) = parse_model_spec("FINAL-Bench_Darwin-36B-Opus-Q6_K");
        assert_eq!(p, "local");
        assert_eq!(m, "FINAL-Bench_Darwin-36B-Opus-Q6_K");
    }

    #[test]
    fn parse_prefixed_splits_on_first_colon() {
        let (p, m) = parse_model_spec("anthropic:claude-sonnet-4-5");
        assert_eq!(p, "anthropic");
        assert_eq!(m, "claude-sonnet-4-5");
    }

    #[test]
    fn parse_empty_provider_treated_as_bare() {
        let (p, m) = parse_model_spec(":weird-model:with-colons");
        assert_eq!(p, "local");
        assert_eq!(m, ":weird-model:with-colons");
    }

    #[test]
    fn registry_synthesizes_local_when_config_absent() {
        // HOME is process-wide; serialize via the shared lock so we
        // don't race other tests that also redirect HOME for fixtures.
        // Without this, a parallel `scoped_home()`-using test was
        // observing a stomped HOME and producing
        // `Invalid input: no enrichment config for corpus 'test-book'`.
        let _home = crate::enrich_cmd::test_env::scoped_home();
        let reg = ProviderRegistry::load_default("http://localhost:9741");
        let local = reg.get("local").unwrap();
        assert_eq!(local.kind, ProviderKind::OpenaiCompatible);
        // The synthesizer auto-appends `/v1` when the caller passes a
        // bare host so the dispatcher's `{base_url}/chat/completions`
        // template lands at `/v1/chat/completions`. Pinned because
        // shipping a bare host is the common atlas-config shape.
        assert_eq!(local.base_url, "http://localhost:9741/v1");
    }

    #[test]
    fn registry_does_not_double_append_v1() {
        let _home = crate::enrich_cmd::test_env::scoped_home();
        let reg = ProviderRegistry::load_default("http://localhost:9741/v1");
        let local = reg.get("local").unwrap();
        assert_eq!(local.base_url, "http://localhost:9741/v1");
    }

    #[test]
    fn synthesized_local_is_not_marked_configured() {
        // The built-in `local` provider must be eligible for capability
        // discovery — it carries a *default*, not an operator override.
        let _home = crate::enrich_cmd::test_env::scoped_home();
        let reg = ProviderRegistry::load_default("http://localhost:9741");
        assert!(!reg.get("local").unwrap().structured_output_configured);
    }

    #[test]
    fn features_json_schema_wins_over_json_object() {
        let f = vec![
            "constraint:json_object".to_string(),
            "constraint:json_schema".to_string(),
        ];
        assert_eq!(
            derive_structured_mode_from_features(&f),
            Some(StructuredOutputMode::JsonSchema)
        );
    }

    #[test]
    fn features_json_object_only() {
        let f = vec!["constraint:json_object".to_string()];
        assert_eq!(
            derive_structured_mode_from_features(&f),
            Some(StructuredOutputMode::JsonObject)
        );
    }

    #[test]
    fn features_without_constraint_keep_default() {
        // A host advertising features but no constraint:* gives no
        // decisive signal — the caller keeps its own default rather
        // than downgrade blindly.
        let f = vec!["think_budget".to_string(), "ingest:v1".to_string()];
        assert_eq!(derive_structured_mode_from_features(&f), None);
        // Empty features (a v0.3 host) likewise.
        assert_eq!(derive_structured_mode_from_features(&[]), None);
    }
}
