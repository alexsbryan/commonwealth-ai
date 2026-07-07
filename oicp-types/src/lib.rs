// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP — Open Inference Capabilities Protocol v0.4.0
//!
//! Canonical types per the specification at
//! `commonwealth/docs/oicp-v0.4.md` (v0.4 extends v0.3 additively;
//! `oicp-v0.3.md` remains the fallback path). Consumed by both the
//! Sovereign and Commonwealth workspaces via path dependency.
//!
//! v0.3 replaces the v0.2 capability-profile vocabulary with
//! specialization-aware routing: capability hints, latency classes,
//! per-model claims. The protocol is intentionally small at launch —
//! two standardized hints (`general`, `code`), three latency classes,
//! and an explicit extension track (`x:<tag>`) for everything else.
//!
//! v0.4 makes a host's constraint machinery and knowledge plane
//! discoverable enough that a client built only against "OICP manifest
//! + OpenAI-compatible HTTP" can run the workflow / recipe-authoring
//! stack against any conforming host: provider-level `features`
//! advertisement (§2), `EmbedModelInfo.query_instruction_prefix` (§4),
//! the ingest extension (§5), and model fingerprints (§6). Every v0.4
//! field is serde-defaulted; an empty v0.4 value serializes identically
//! to a v0.3 manifest.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// OICP specification version implemented by this module.
pub const OICP_VERSION: &str = "0.4.0";

// -----------------------------------------------------------------
// Internal model-metadata vocabulary
//
// `Capability`, `CapabilityProfile`, `ProficiencyLevel`, and
// `proficiency` are **not** on the v0.3 wire. They're the shared
// vocabulary the runtime uses internally to describe what a model is
// good at (sourced from `models.toml`, skill TOML declarations, etc.)
// and to derive claim affinities when synthesizing a `ProviderModel`'s
// `claims` vector for advertisement. Prefer these over ad-hoc
// per-crate enums so the vocabulary stays consistent across the
// Sovereign runtime, Commonwealth's model registry, and skill
// authoring.
// -----------------------------------------------------------------

/// Capability domains a model can be proficient in. Local metadata
/// only — not part of the OICP wire (the wire uses
/// [`CapabilityHint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    General,
    Code,
    Analysis,
    Math,
    Creative,
    Instruction,
    Multilingual,
    Vision,
    LongContext,
    #[serde(other)]
    Unknown,
}

/// Proficiency level on a 0–4 ordinal scale. 0 = None, 1 = Basic,
/// 2 = Moderate, 3 = Strong, 4 = Exceptional.
pub type ProficiencyLevel = u8;

/// A map from capability domains to proficiency levels. Used by the
/// runtime to describe loaded models internally; translated to v0.3
/// [`CapabilityClaim`]s at advertisement time.
pub type CapabilityProfile = HashMap<Capability, ProficiencyLevel>;

/// Returns the proficiency for a capability, defaulting to 0 if
/// absent or if the capability deserialized to `Unknown`.
pub fn proficiency(profile: &CapabilityProfile, cap: Capability) -> ProficiencyLevel {
    if matches!(cap, Capability::Unknown) {
        return 0;
    }
    profile.get(&cap).copied().unwrap_or(0)
}

/// Derive a [`CapabilityHint`] from a [`CapabilityProfile`].
///
/// A model is considered code-specialized only when its `code`
/// proficiency is `Exceptional` (4) **and** that proficiency
/// strictly exceeds its `general` proficiency — i.e., the model is
/// meaningfully better at code than at general work. Everything else
/// maps to `general`.
///
/// This is deliberately strict, mirroring v0.3 §4.4 ("start only
/// with clearly differentiated specializations"). A well-rounded 9B
/// with `general: 3, code: 3, analysis: 3` is **not** a code
/// specialist; it handles code adequately alongside everything else
/// and should route on the general hint. Only a model that is
/// measurably better at code than general — Qwen Coder, DeepSeek
/// Coder, Code Llama variants — earns the `code` hint.
pub fn infer_hint_from_profile(profile: &CapabilityProfile) -> CapabilityHint {
    let code = proficiency(profile, Capability::Code);
    let general = proficiency(profile, Capability::General);
    if code == 4 && code > general {
        CapabilityHint::code()
    } else {
        CapabilityHint::general()
    }
}

// -----------------------------------------------------------------
// v0.3 §4 — Capability Hints
// -----------------------------------------------------------------

/// A capability hint naming a kind of inference work (§4).
///
/// Two hints are standardized at protocol launch: `general` and `code`.
/// Any other specialization (prose, biomedical, math, dialogue, …)
/// starts as an extension hint, which must carry the `x:` prefix so it
/// is distinguishable at parse time from a standardized hint.
///
/// # Forward compatibility
///
/// Parsing is deliberately permissive: any non-empty, whitespace-free
/// string is accepted. A bare hint that isn't currently standardized
/// in this build (e.g., because a future spec promoted `math` to
/// standardized and this client predates the bump) is preserved
/// verbatim. The scheduler may still match it by exact string against
/// advertised claims; where no match exists it falls back to `general`
/// per §4.2.
///
/// # Construction
///
/// Use [`CapabilityHint::general`] and [`CapabilityHint::code`] for
/// the standardized constants, [`CapabilityHint::extension`] to wrap
/// an extension tag (the `x:` prefix is applied automatically), and
/// [`CapabilityHint::parse`] to validate a raw string received over
/// the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityHint(String);

impl CapabilityHint {
    /// Standardized hint: no specific specialization target. Every
    /// node serving inference supports this hint; every request
    /// without a more specific hint uses this (§4.1).
    pub const GENERAL: &'static str = "general";

    /// Standardized hint: code generation, understanding,
    /// modification, review. Models tuned for code emphasis serve
    /// this with higher affinity than general-purpose models (§4.1).
    pub const CODE: &'static str = "code";

    /// Extension prefix: extension hints must start with `x:` (§4.2).
    /// Chosen over `ext/` or `@ns/` for brevity and because `:`
    /// never appears in a standardized hint.
    pub const EXTENSION_PREFIX: &'static str = "x:";

    /// Known standardized hints as of this crate build. Grows by
    /// governance decision (§4.3); do not extend locally.
    pub const STANDARDIZED: &'static [&'static str] = &[Self::GENERAL, Self::CODE];

    /// The standardized `general` hint.
    pub fn general() -> Self {
        Self(Self::GENERAL.to_string())
    }

    /// The standardized `code` hint.
    pub fn code() -> Self {
        Self(Self::CODE.to_string())
    }

    /// Wrap `tag` as an extension hint, prepending the `x:` prefix.
    ///
    /// Rejects empty/whitespace tags, tags that already carry the
    /// prefix, tags that collide with a standardized hint, and tags
    /// containing whitespace.
    pub fn extension(tag: impl AsRef<str>) -> Result<Self, InvalidCapabilityHint> {
        let tag = tag.as_ref().trim();
        if tag.is_empty() {
            return Err(InvalidCapabilityHint::Empty);
        }
        if tag.starts_with(Self::EXTENSION_PREFIX) {
            return Err(InvalidCapabilityHint::AlreadyPrefixed);
        }
        if tag.chars().any(|c| c.is_whitespace()) {
            return Err(InvalidCapabilityHint::Whitespace);
        }
        if Self::STANDARDIZED.contains(&tag) {
            return Err(InvalidCapabilityHint::CollidesWithStandardized);
        }
        Ok(Self(format!("{}{}", Self::EXTENSION_PREFIX, tag)))
    }

    /// Parse a raw hint string (e.g., from a manifest or request).
    ///
    /// Accepts any non-empty, whitespace-free string. Rejection is
    /// reserved for structurally unusable values — bare strings that
    /// aren't currently standardized here are preserved verbatim so
    /// forward-compatible matching still works (§10.3).
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, InvalidCapabilityHint> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(InvalidCapabilityHint::Empty);
        }
        if raw.chars().any(|c| c.is_whitespace()) {
            return Err(InvalidCapabilityHint::Whitespace);
        }
        Ok(Self(raw.to_string()))
    }

    /// The hint's wire form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True iff this hint is in [`STANDARDIZED`](Self::STANDARDIZED).
    pub fn is_standardized(&self) -> bool {
        Self::STANDARDIZED.iter().any(|s| *s == self.0)
    }

    /// True iff this hint carries the `x:` extension prefix and has
    /// a non-empty tag component.
    pub fn is_extension(&self) -> bool {
        self.0.starts_with(Self::EXTENSION_PREFIX) && self.0.len() > Self::EXTENSION_PREFIX.len()
    }

    /// True iff this hint is neither standardized in this build nor
    /// an extension — likely a future-standardized hint. Schedulers
    /// should still attempt exact-string matches against advertised
    /// claims before falling back to `general`.
    pub fn is_unknown_bare(&self) -> bool {
        !self.is_standardized() && !self.is_extension()
    }
}

impl std::fmt::Display for CapabilityHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reasons [`CapabilityHint`] construction can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidCapabilityHint {
    /// The input was empty or whitespace-only.
    Empty,
    /// The extension tag was given with the `x:` prefix already
    /// attached — [`CapabilityHint::extension`] adds the prefix.
    AlreadyPrefixed,
    /// The input contained internal whitespace.
    Whitespace,
    /// The extension tag equals a standardized hint; use
    /// [`CapabilityHint::general`] / [`CapabilityHint::code`].
    CollidesWithStandardized,
}

impl std::fmt::Display for InvalidCapabilityHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Empty => "capability hint is empty",
            Self::AlreadyPrefixed => "capability hint already carries the 'x:' extension prefix",
            Self::Whitespace => "capability hint contains whitespace",
            Self::CollidesWithStandardized => "extension tag collides with a standardized hint",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for InvalidCapabilityHint {}

impl Serialize for CapabilityHint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CapabilityHint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

// -----------------------------------------------------------------
// v0.3 §5 — Latency Classes
// -----------------------------------------------------------------

/// Time-sensitivity class for a request or a node's typical response
/// time for a kind of work (§5).
///
/// These are categories, not precise SLAs. A node advertises the
/// class matching its typical behaviour; a request names the class
/// it needs. The scheduler prefers matching classes and treats
/// mismatch as a soft deprioritization rather than a hard failure
/// (§6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    /// Time-to-first-token in hundreds of milliseconds. Suitable for
    /// routing, classification, short extractions, interactive UI.
    Fast,
    /// Time-to-first-token in single-digit seconds. Suitable for
    /// most substantive inference work. Default.
    #[default]
    Normal,
    /// TTFT may be longer; total generation may span tens of
    /// seconds or more. Suitable for reasoning-heavy work,
    /// long-context synthesis, deep planning.
    Extended,
}

// -----------------------------------------------------------------
// v0.3 §3.1 — Capability Claims
// -----------------------------------------------------------------

/// A capability claim: what kind of inference work this node serves,
/// with a stated affinity (§3.1).
///
/// A node may publish multiple claims — one per (model, latency
/// class) combination, or one per model for nodes running a single
/// model. A claim is the unit of scheduling: the scheduler ranks
/// (node, claim) pairs against a request's property set (§6).
///
/// `affinity` is the node's self-assessment of how well it serves
/// this kind of work, clamped to `[0.0, 1.0]` with documented
/// reference points:
///
/// - `1.0` — exceptional fit (specialized model serving its specialty).
/// - `~0.85` — strong fit (large general model serving general work).
/// - `~0.7` — solid fit (small fast model serving fast work well).
/// - `~0.5` — adequate fit (model can serve but prefer elsewhere).
/// - below `0.5` — feasible but substantially degraded.
///
/// Affinity is self-reported and therefore less reliable than
/// structural facts (context / output capacity, hint match). The
/// scheduler treats it as a tiebreaker, not a primary ranker.
///
/// **Reality note (so maintainers aren't surprised):** in this
/// codebase the advertised affinity is a STATIC value derived from
/// model-profile config at startup (`models.toml` proficiencies ÷ 4)
/// — there is no feedback loop that re-advertises a different
/// affinity after observing failures. The "observed health" the
/// spec's §7 describes happens entirely SCORER-side, at request
/// time, via [`effective_affinity`] inside
/// [`score_with_adjustments`]; peers always see the original claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityClaim {
    /// The kind of work this claim covers.
    pub hint: CapabilityHint,
    /// The latency class this claim covers.
    pub latency_class: LatencyClass,
    /// Maximum context (input) tokens the model accepts for this claim.
    pub max_context: u32,
    /// Maximum output tokens the model will produce for this claim.
    pub max_output: u32,
    /// Self-assessed affinity in `[0.0, 1.0]`. Accessed via
    /// [`CapabilityClaim::effective_affinity`] to apply the clamp.
    pub affinity: f32,
}

impl CapabilityClaim {
    /// Construct a claim. `affinity` is stored verbatim; use
    /// [`effective_affinity`](Self::effective_affinity) for the
    /// clamped-to-`[0.0, 1.0]` value used in scheduling.
    pub fn new(
        hint: CapabilityHint,
        latency_class: LatencyClass,
        max_context: u32,
        max_output: u32,
        affinity: f32,
    ) -> Self {
        Self {
            hint,
            latency_class,
            max_context,
            max_output,
            affinity,
        }
    }

    /// Affinity clamped to `[0.0, 1.0]` per §3.1 semantics. NaN
    /// collapses to 0.0.
    pub fn effective_affinity(&self) -> f32 {
        if self.affinity.is_nan() {
            0.0
        } else {
            self.affinity.clamp(0.0, 1.0)
        }
    }

    /// True iff this claim can fit the structural size of a request
    /// (§6 — context/output are hard constraints).
    pub fn fits(&self, context_tokens: u32, max_output_tokens: u32) -> bool {
        self.max_context >= context_tokens && self.max_output >= max_output_tokens
    }
}

// -----------------------------------------------------------------
// Section 3 — Client Requirements Schema
// -----------------------------------------------------------------

/// What a client needs from an inference call (§3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequirements {
    pub oicp_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<PrivacyRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// §3.2: capability hint for specialization-aware routing.
    /// Absent → scheduler treats as `general` per §8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_hint: Option<CapabilityHint>,
    /// §3.2: latency class the request needs. Absent → scheduler
    /// treats as [`LatencyClass::Normal`] per §8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_class: Option<LatencyClass>,
    /// §3.2: actual context length of the request. Used by the
    /// scheduler as a hard feasibility gate against each claim's
    /// `max_context` (§6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// §3.2: expected output length. Used by the scheduler as a
    /// hard feasibility gate against each claim's `max_output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl Default for InferenceRequirements {
    fn default() -> Self {
        Self {
            oicp_version: OICP_VERSION.to_string(),
            privacy: None,
            request_id: None,
            capability_hint: None,
            latency_class: None,
            context_tokens: None,
            max_output_tokens: None,
        }
    }
}

impl InferenceRequirements {
    /// New empty requirements at the current OICP version.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set the sharding privacy. Allocates `privacy` if absent.
    pub fn with_sharding(mut self, sharding: ShardingPrivacy) -> Self {
        self.privacy = Some(PrivacyRequirements { sharding });
        self
    }

    /// Builder: set the request id.
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// Builder: set the capability hint.
    pub fn with_hint(mut self, hint: CapabilityHint) -> Self {
        self.capability_hint = Some(hint);
        self
    }

    /// Builder: set the latency class.
    pub fn with_latency_class(mut self, class: LatencyClass) -> Self {
        self.latency_class = Some(class);
        self
    }

    /// Builder: set the actual context length.
    pub fn with_context_tokens(mut self, tokens: u32) -> Self {
        self.context_tokens = Some(tokens);
        self
    }

    /// Builder: set the expected output length.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// §8: effective hint, defaulting to `general` when absent.
    pub fn effective_hint(&self) -> CapabilityHint {
        self.capability_hint
            .clone()
            .unwrap_or_else(CapabilityHint::general)
    }

    /// §8: effective latency class, defaulting to `Normal`.
    pub fn effective_latency_class(&self) -> LatencyClass {
        self.latency_class.unwrap_or(LatencyClass::Normal)
    }

    /// Effective sharding privacy, defaulting to `LocalOnly` per §3.1.
    pub fn sharding(&self) -> ShardingPrivacy {
        self.privacy
            .as_ref()
            .map(|p| p.sharding)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrivacyRequirements {
    #[serde(default)]
    pub sharding: ShardingPrivacy,
}

/// Whether the provider may distribute inference across multiple
/// nodes (§3.1).
///
/// Default is `LocalOnly`. The spec calls this out explicitly:
/// "privacy is the default, not something the client has to
/// remember to request." Clients that want distributed inference
/// must opt in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardingPrivacy {
    #[default]
    LocalOnly,
    MeshAllowed,
}

// -----------------------------------------------------------------
// v0.4 §2.1 — Feature advertisement vocabulary
// -----------------------------------------------------------------

/// Registered feature strings for [`ProviderManifest::features`] (v0.4
/// §2.1). A host advertises the request-level capabilities it honours
/// so a decoupled client can negotiate (§3) instead of guessing.
/// Extension features carry the `x:` prefix; unknown features are
/// preserved verbatim and treated as absent by clients.
pub mod features {
    /// `response_format: {type: "json_schema"}` is grammar-enforced —
    /// output is guaranteed to validate against the supplied schema.
    pub const CONSTRAINT_JSON_SCHEMA: &str = "constraint:json_schema";
    /// `response_format: {type: "json_object"}` guarantees syntactically
    /// valid JSON (no schema-conformance guarantee).
    pub const CONSTRAINT_JSON_OBJECT: &str = "constraint:json_object";
    /// The `lark_grammar` body field is honoured; output is guaranteed
    /// to be in the grammar's language. More expressive than JSON Schema.
    pub const CONSTRAINT_LARK: &str = "constraint:lark";
    /// The `url_allowlist` sampler constraint is honoured.
    pub const CONSTRAINT_ALLOWLIST_URL: &str = "constraint:allowlist:url";
    /// The `evidence_id_allowlist` sampler constraint is honoured.
    pub const CONSTRAINT_ALLOWLIST_EVIDENCE_ID: &str = "constraint:allowlist:evidence_id";
    /// The `cmd_prefix` / `assistant_prefix` sampler constraints are honoured.
    pub const CONSTRAINT_ALLOWLIST_CMD_PREFIX: &str = "constraint:allowlist:cmd_prefix";
    /// The `think_budget` body field (a reasoning-token cap) is honoured.
    pub const THINK_BUDGET: &str = "think_budget";
    /// The `oicp` request envelope ([`InferenceRequirements`]) is
    /// consumed for routing.
    pub const OICP_REQUEST_PROPERTIES: &str = "oicp:request_properties";
    /// The §5 ingest extension (install + progress) is mounted; MUST
    /// co-occur with a populated `knowledge.ingest`.
    pub const INGEST_V1: &str = "ingest:v1";
    /// The §5.4 recipe-test endpoint is mounted; MUST co-occur with
    /// `knowledge.ingest.test_endpoint`.
    pub const INGEST_RECIPE_TEST: &str = "ingest:recipe_test";
    /// §6 fingerprints are populated on manifest models and echoed in
    /// response metadata.
    pub const MODEL_FINGERPRINT: &str = "model_fingerprint";

    /// Extension-feature prefix (§2.1). A host MAY advertise
    /// `x:`-prefixed features not registered in this crate build.
    pub const EXTENSION_PREFIX: &str = "x:";

    /// Every feature this crate build knows how to name. Grows by spec
    /// revision; a host MAY advertise `x:`-prefixed features not listed.
    pub const REGISTERED: &[&str] = &[
        CONSTRAINT_JSON_SCHEMA,
        CONSTRAINT_JSON_OBJECT,
        CONSTRAINT_LARK,
        CONSTRAINT_ALLOWLIST_URL,
        CONSTRAINT_ALLOWLIST_EVIDENCE_ID,
        CONSTRAINT_ALLOWLIST_CMD_PREFIX,
        THINK_BUDGET,
        OICP_REQUEST_PROPERTIES,
        INGEST_V1,
        INGEST_RECIPE_TEST,
        MODEL_FINGERPRINT,
    ];

    /// True iff `f` is a registered feature string or a well-formed
    /// `x:`-prefixed extension feature (non-empty tag). This is the
    /// validity predicate the conformance suite's `manifest.features`
    /// check applies.
    pub fn is_valid(f: &str) -> bool {
        REGISTERED.contains(&f)
            || f.strip_prefix(EXTENSION_PREFIX)
                .is_some_and(|tag| !tag.is_empty())
    }
}

// -----------------------------------------------------------------
// Section 4 — Provider Manifest Schema
// -----------------------------------------------------------------

/// Provider manifest served at `GET /oicp/v1/capabilities` (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderManifest {
    pub oicp_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderInfo>,
    pub models: Vec<ProviderModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<KnowledgeManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub federation: Option<FederationManifest>,
    /// v0.4 §2: request-level capabilities this host honours. Empty
    /// (the serde default and the absence-on-the-wire shape) means
    /// "v0.3 host" — the client assumes only baseline OpenAI-compat.
    /// See the [`features`] module for registered strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl ProviderManifest {
    pub fn new(models: Vec<ProviderModel>) -> Self {
        Self {
            oicp_version: OICP_VERSION.to_string(),
            provider: None,
            models,
            knowledge: None,
            federation: None,
            features: Vec::new(),
        }
    }

    /// True iff this manifest advertises feature `f` (§2).
    pub fn has_feature(&self, f: &str) -> bool {
        self.features.iter().any(|x| x == f)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub provider_type: Option<ProviderType>,
}

/// Provider type hint (§4.1). Informational only — clients MUST NOT
/// make routing decisions based on this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Local,
    Mesh,
    Cloud,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    pub context_tokens: u32,
    pub status: ModelStatus,
    /// Approximate on-disk weight size in gigabytes. Used as a
    /// tiebreaker during OICP backend selection: when two models
    /// score equally against a request, prefer the smaller one
    /// (smaller ≈ faster TTFT, lighter memory footprint, less
    /// energy). Optional because providers may not know or want to
    /// publish this; absent values sort after any known value so an
    /// unknown-size model never spuriously wins a tie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_gb: Option<f32>,
    /// §4: capability claims advertised for this model. Each claim
    /// describes a (capability hint × latency class × context ×
    /// output × affinity) combination the model serves well.
    /// Multiple claims per model are expected when a single model
    /// handles more than one latency class (e.g., a 9B general
    /// model serving both fast short-context and normal long-context
    /// work).
    pub claims: Vec<CapabilityClaim>,
    /// v0.4 §6: opaque fingerprint that MUST change when the served
    /// weights, quantization, or chat template change. Lets a client
    /// key model-dependent caches on `(id, fingerprint)`. Gated by the
    /// `model_fingerprint` feature; absent on v0.3 hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub available: bool,
    pub loaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_per_sec: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_ttft_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_load_time_sec: Option<u32>,
}

// -----------------------------------------------------------------
// Embed model compatibility (used by collaborative ingestion)
// -----------------------------------------------------------------

/// How token embeddings are pooled into a single sequence embedding.
/// Matches the values used by sovereign-core's EmbedQuirks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolingStrategy {
    /// Last non-padding token hidden state (Qwen3-Embedding).
    Last,
    /// Average all non-padding hidden states (mxbai, BERT-style embedders).
    Mean,
    /// [CLS] token hidden state (BERT-style models).
    Cls,
}

/// Whether L2 normalisation is performed by the server or the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationStrategy {
    /// llama-server normalises via --embd-normalize.
    Server,
    /// Application must L2-normalise the raw vector before use.
    Application,
}

/// Embedding model identity and output shape.
/// Two nodes are compatible for collaborative ingestion iff their
/// `EmbedModelInfo` values are equal (exact match required — cosine
/// similarity across different embedding spaces is meaningless). The
/// v0.4 `query_instruction_prefix` is part of that equality: it changes
/// the query-side embedding space, so two nodes with different prefixes
/// are incompatible even when the other four fields match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbedModelInfo {
    /// Model identifier, e.g. `"qwen3-embedding-0.6b"`.
    pub model_id: String,
    /// Output vector dimensionality.
    pub dimensions: usize,
    pub pooling: PoolingStrategy,
    pub normalization: NormalizationStrategy,
    /// v0.4 §4: instruction prefix prepended to *query* text (not
    /// document text) before embedding. Empty string = no prefix (also
    /// the v0.3-on-the-wire shape via serde default). A client
    /// reconstructing a query embedding for federated search MUST
    /// prepend this or it produces a vector in a different space.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query_instruction_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeManifest {
    pub corpora: Vec<CorpusDescriptor>,
    pub search_endpoint: String,
    /// Embed model in use on this node. `None` means the node has
    /// not advertised its embed configuration — exclude from
    /// collaborative ingestion until this is populated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<EmbedModelInfo>,
    /// v0.4 §5: corpus-ingest endpoints this host exposes. `None` means
    /// the host does not offer an OICP ingest surface. When present,
    /// the manifest MUST also advertise the `ingest:v1` feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest: Option<IngestEndpoints>,
}

/// v0.4 §5: the corpus-ingest endpoints advertised in
/// [`KnowledgeManifest::ingest`]. Values are paths relative to the
/// manifest's origin (the same convention as `search_endpoint`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEndpoints {
    /// `POST` — install a corpus by recipe id. See [`CorpusInstallRequest`].
    pub install_endpoint: String,
    /// `GET` — poll ingest progress. See [`CorpusProgressResponse`].
    pub progress_endpoint: String,
    /// `POST` — optional dry-run recipe test (§5.4). Present iff the
    /// host advertises the `ingest:recipe_test` feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusDescriptor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub total_chunks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shards: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u32>,
    pub fully_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationManifest {
    pub peers: Vec<PeerDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerDescriptor {
    pub name: String,
    pub capabilities_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
}

// -----------------------------------------------------------------
// Section 5.2 — Response Metadata
// -----------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OicpResponseMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_quality: Option<MatchQuality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// v0.4 §6: fingerprint of the concrete model that produced this
    /// response — the same token as the resolved model's
    /// [`ProviderModel::fingerprint`]. Lets a client key model-dependent
    /// caches correctly across a model swap. Gated by the
    /// `model_fingerprint` feature; absent on v0.3 hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchQuality {
    Full,
    Partial,
    Degraded,
    Unmatched,
}

// -----------------------------------------------------------------
// Section 6 — Knowledge Search API
// -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchRequest {
    /// Pre-computed query embedding. OPTIONAL as of v0.4: when empty,
    /// the HOST embeds `query_text` with its advertised
    /// [`EmbedModelInfo::query_instruction_prefix`] — the OICP contract
    /// is thin-client (the host owns the embed model), so a client need
    /// only send text. Mesh peers still pre-embed and send this to
    /// avoid re-embedding on every hop; when present it is used as-is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_embedding: Vec<f32>,
    /// The query text. `query` is accepted as an alias — it is the
    /// natural OICP thin-client field name; `query_text` is retained
    /// for the mesh-internal shape.
    #[serde(default, alias = "query")]
    pub query_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpora: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl KnowledgeSearchRequest {
    /// The default result limit per §6.1 when `limit` is omitted.
    pub const DEFAULT_LIMIT: u32 = 20;

    /// Effective result limit, applying the §6.1 default of 20.
    pub fn effective_limit(&self) -> u32 {
        self.limit.unwrap_or(Self::DEFAULT_LIMIT)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeSearchResponse {
    pub results: Vec<KnowledgeResult>,
    pub corpora_searched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora_unavailable: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_chunks_searched: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeResult {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub corpus_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub score: f32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    /// Stable LanceDB row id for the chunk on the producing peer.
    /// Lets the desktop's reading surface deref a citation back to
    /// the source chunk (see ENRICHMENT_V2 / glass-box reading
    /// surface plan). `None` for synthetic chunks (atlas-virtual,
    /// local-doc) and for older peers that haven't been upgraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<u64>,
    /// Document grouping key for "elsewhere in this document"
    /// lookups and for chunk-neighbor ordering. `None` when the
    /// extractor didn't tag chunks with a document id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_doc_id: Option<String>,
}

// -----------------------------------------------------------------
// Section 7 — Ingest Extension (v0.4 §5)
// -----------------------------------------------------------------

/// `POST {install_endpoint}` — install a corpus by recipe id (§5.1).
/// Idempotent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusInstallRequest {
    pub corpus_id: String,
    /// Recipe `[parameters]` values, keyed by parameter name. Empty map
    /// when the recipe takes no parameters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, serde_json::Value>,
}

/// Response to [`CorpusInstallRequest`] (§5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusInstallResponse {
    pub corpus_id: String,
    /// `true` — a fresh ingest job started. `false` — the corpus is
    /// already installed or an ingest for it is already running.
    pub spawned: bool,
}

/// Coarse ingest phase (§5.2). A protocol type — deliberately does not
/// embed any implementation's internal progress enum, so a host may
/// implement ingest without linking the reference engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestPhase {
    Pending,
    Downloading,
    Embedding,
    Indexing,
    Optimizing,
    Enriching,
    Complete,
    Failed,
}

impl IngestPhase {
    /// True for `Complete` and `Failed` — the terminal phases of the
    /// §5.3 poll state machine.
    pub fn is_terminal(self) -> bool {
        matches!(self, IngestPhase::Complete | IngestPhase::Failed)
    }
}

/// Per-corpus ingest progress (§5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusIngestProgress {
    pub phase: IngestPhase,
    /// Best-effort completion fraction in `[0,1]`; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
    /// Human-readable detail; the error message when `phase = Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `GET {progress_endpoint}` response (§5.2). Keyed by `corpus_id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorpusProgressResponse {
    #[serde(default)]
    pub progress: BTreeMap<String, CorpusIngestProgress>,
}

/// `POST {test_endpoint}` — dry-run a recipe over a small sample (§5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeTestRequest {
    /// The full recipe TOML source.
    pub recipe_toml: String,
    #[serde(default)]
    pub options: RecipeTestOptions,
}

/// Options for [`RecipeTestRequest`] (§5.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeTestOptions {
    /// Cap the number of documents pulled per stage; `None` = host default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_limit: Option<u32>,
    /// Skip any network acquisition (test extract/chunk over cached input).
    #[serde(default)]
    pub offline: bool,
}

/// Per-stage diagnostics from a recipe test (§5.4). A protocol type —
/// no implementation internals on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeStageReport {
    /// Stage name, e.g. `"acquire"`, `"extract"`, `"chunk"`.
    pub name: String,
    pub docs_in: u32,
    pub docs_out: u32,
    /// Things the stage expected but did not find (e.g. missed sections).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub misses: Vec<String>,
    /// A few sample outputs, for the author to eyeball.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample: Vec<String>,
}

/// Response to [`RecipeTestRequest`] (§5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeTestReport {
    pub stages: Vec<RecipeStageReport>,
    /// `true` iff every stage produced output (a usable recipe).
    pub ok: bool,
}

// -----------------------------------------------------------------
// Section 6.5 — Knowledge Landscape Digest API
// -----------------------------------------------------------------
//
// The daemon-side `KnowledgeViewManager` exposes its assembled
// digests via `POST /v1/knowledge/landscape_digest`, so an attached
// desktop (which does NOT construct its own manager — see
// `AppState::is_attach_mode`) can splice the same prompt blocks the
// daemon would. Wire shape mirrors the existing
// `LandscapeDigest` type in `sovereign-core::types`; we redefine it
// here to keep `oicp-types` a leaf crate with no upstream Sovereign
// deps. The receiving side maps between the two.

/// One assembled landscape-digest block (e.g. personal-knowledge,
/// conversation-history, cross-view, relational, strategic). The
/// `body` is markdown ready to splice; the `view_id` lets clients
/// dedupe / re-order if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LandscapeDigestEntry {
    pub view_id: String,
    pub body: String,
}

/// Request body for `POST /v1/knowledge/landscape_digest`. All
/// fields are optional — the simplest valid request is `{}`,
/// equivalent to "give me the unconstrained digest set with no
/// active-skill privacy filter and no in-conversation context."
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LandscapeDigestRequest {
    /// Active skill id. Today this is informational only; reserved
    /// for v2 skill-tiered digest work. The daemon does NOT
    /// introspect it for privacy gating — see
    /// `active_is_local_only` for that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_skill: Option<String>,
    /// Caller-resolved "the active skill has privacy = local_only"
    /// flag. The desktop has the canonical skill registry and
    /// computes this against `SkillRegistry::local_only_skill_ids`;
    /// the daemon trusts the flag and applies it directly. This
    /// design keeps the daemon out of the skill-registry business
    /// while preserving the splice-time privacy filter (a
    /// `local_only` session must NOT receive
    /// conversational/institutional/cross-view blocks).
    #[serde(default)]
    pub active_is_local_only: bool,
    /// In-conversation message contents. Drives the "this entity is
    /// already on screen, don't re-introduce it" predicate in the
    /// relational/strategic blocks. Empty = no in-conv suppression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation_messages: Vec<String>,
}

/// Response shape — a flat list of digests in the order the daemon
/// would have spliced them. The desktop calls
/// `ConversationContext::set_landscape_digests` with the converted
/// list and the runtime treats it identically to a locally-spliced
/// payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LandscapeDigestResponse {
    pub digests: Vec<LandscapeDigestEntry>,
}

// -----------------------------------------------------------------
// v0.3 §6 — Reference scoring function
// -----------------------------------------------------------------

/// Hint-match score when a request asks for a specific hint but only
/// a `general` claim is available. Decisively worse than an exact
/// match (1.0) yet noticeably better than a wrong specialization
/// (0.0) so the scheduler prefers any node with the requested
/// specialty over a general fallback, but still routes work
/// somewhere if no specialist is reachable.
pub const HINT_GENERAL_FALLBACK_SCORE: f32 = 0.5;

/// Latency-match score when claim and request classes are one class
/// apart (fast↔normal or normal↔extended). Latency mismatch is a
/// soft deprioritization per §5 — a node advertising fast work can
/// still serve normal work, just with a weaker fit.
///
/// The values 0.8 / 0.5 are NOT derived from spec §5 (which mandates
/// only "soft deprioritization") — they are this reference
/// implementation's choices, sized so one class of mismatch loses to
/// any same-class claim within ~0.25 affinity, and two classes lose
/// to anything plausible. Pinned by tests for scheduler interop;
/// change them only with a routing A/B in hand.
pub const LATENCY_ADJACENT_SCORE: f32 = 0.8;

/// Latency-match score when claim and request classes are two apart
/// (fast↔extended). The widest soft deprioritization. Same
/// non-normative-but-pinned status as [`LATENCY_ADJACENT_SCORE`].
pub const LATENCY_TWO_CLASS_SCORE: f32 = 0.5;

/// Score how well a claim's `hint` covers a request for `req_hint`.
///
/// - Exact match (same standardized hint, or same extension hint) →
///   `1.0`.
/// - Request specific (e.g., `code`, `x:prose`), claim `general` →
///   [`HINT_GENERAL_FALLBACK_SCORE`] (0.5) — the documented spec
///   §4.2 fallback: "falling back to general when no node advertises
///   the requested hint."
/// - Every other non-match → `0.0`. In particular, a request for
///   `general` against a specific-hint claim (code, x:prose, …) is
///   **not** a free 1.0. The spec §4.1 requirement "every node
///   serving inference must support general as a minimum" is an
///   obligation on the **advertiser**: a node that wants to serve
///   general work must publish a general claim. Scoring a code-
///   specialist claim at 1.0 for a general request would subvert
///   that obligation and let a specialist silently absorb every
///   general-hinted request on the mesh.
pub fn hint_match_score(claim_hint: &CapabilityHint, req_hint: &CapabilityHint) -> f32 {
    if claim_hint == req_hint {
        return 1.0;
    }
    // Request asks for a specific hint; claim offers general —
    // documented fallback path (§4.2).
    if claim_hint.as_str() == CapabilityHint::GENERAL
        && req_hint.as_str() != CapabilityHint::GENERAL
    {
        return HINT_GENERAL_FALLBACK_SCORE;
    }
    // All other mismatches (request general vs specific claim; two
    // different specifics) are zero score → eliminated from ranking
    // by the scheduler.
    0.0
}

/// Score how well a claim's `latency_class` covers a request for
/// `req_class`.
///
/// - Exact match → `1.0`.
/// - Adjacent class → [`LATENCY_ADJACENT_SCORE`] (0.8).
/// - Two-class gap → [`LATENCY_TWO_CLASS_SCORE`] (0.5).
pub fn latency_match_score(claim_class: LatencyClass, req_class: LatencyClass) -> f32 {
    fn rank(c: LatencyClass) -> i32 {
        match c {
            LatencyClass::Fast => 0,
            LatencyClass::Normal => 1,
            LatencyClass::Extended => 2,
        }
    }
    match rank(claim_class).abs_diff(rank(req_class)) {
        0 => 1.0,
        1 => LATENCY_ADJACENT_SCORE,
        _ => LATENCY_TWO_CLASS_SCORE,
    }
}

// -----------------------------------------------------------------
// v0.3 §7 — Operational state (non-normative)
//
// The spec explicitly leaves observation, load, and locality
// modelling to each scheduler (§7 "operational concerns are local").
// These types + helpers are the shared reference model so Sovereign
// + Commonwealth + mesh-peer schedulers all rank (node, claim) pairs
// with the same second-pass scoring math. Nothing here is on the
// wire.
// -----------------------------------------------------------------

/// Sample-count threshold above which observed-performance fully
/// replaces claimed affinity in [`effective_affinity`]. Below this
/// the claim still dominates; at this value and above the observed
/// health score fully applies.
pub const CONFIDENCE_SAMPLES: u32 = 50;

/// Sample threshold for cold-start ramping in [`cold_start_weight`].
/// A brand-new node starts at [`COLD_START_MIN_WEIGHT`] and ramps
/// linearly to `1.0` over this many observed samples.
pub const COLD_START_SAMPLES: u32 = 20;

/// Minimum routing weight a brand-new node gets before any
/// observations exist. Non-trivially below `1.0` so new peers
/// don't absorb a burst before they've proven reliable, but high
/// enough that a peer with a strictly-better advertised affinity
/// can still win the first request — otherwise the scheduler
/// would never actually ROUTE to new peers and cold-start would
/// become a trap. `0.7` corresponds to "new peer gets 70% of the
/// weight it would at full ramp", roughly the same deprioritization
/// a real-world load balancer uses for fresh backends.
pub const COLD_START_MIN_WEIGHT: f32 = 0.7;

/// Load-penalty coefficient: `load_penalty = 1 / (1 + in_flight * C)`.
/// At the default 0.05, 5 in-flight requests drop the penalty to
/// ~0.8; 20 in-flight drops to ~0.5 — enough to divert the next
/// burst to a second-choice node without starving the popular one.
pub const LOAD_COEFFICIENT: f32 = 0.05;

/// Locality bonus: same-machine local serving.
pub const LOCALITY_LOCAL_BONUS: f32 = 1.15;

/// Locality bonus: same-LAN peer.
pub const LOCALITY_NEAR_BONUS: f32 = 1.05;

/// Locality bonus: cross-internet peer (no bonus).
pub const LOCALITY_FAR_BONUS: f32 = 1.0;

/// Reference token-generation rate that maps to a throughput factor of
/// `1.0` in [`throughput_factor`]. Anything at or above this rate is
/// treated as fully responsive; lower rates scale linearly down toward
/// the floor. 20 tok/s is the "good for interactive use" inflection
/// point — below it conversation feels sluggish to a human.
pub const THROUGHPUT_REFERENCE_TG_TOK_S: f32 = 20.0;

/// Floor for [`throughput_factor`]: a node observed at very low
/// throughput is still routable as a last resort. Without a floor a
/// 3 tok/s peer would score `0.15×` and effectively never receive
/// traffic, even when it is the only candidate that satisfies the
/// hard gates. The floor preserves reachability while still tilting
/// routing decisively toward faster peers.
pub const THROUGHPUT_FLOOR: f32 = 0.3;

/// Where a node sits relative to the scheduler making the routing
/// decision. Derived from the scheduler's network topology — not
/// advertised by the peer. Protocol-independent: every scheduler
/// resolves its own `(peer → locality)` map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NodeLocality {
    /// Same process or machine. No network hop.
    Local,
    /// Same LAN. Single-digit-ms hop.
    Near,
    /// Cross-internet. Tens of ms hop, up to hundreds for relayed
    /// paths. Default for unknown peers.
    #[default]
    Far,
}

/// Per-node operational observations recorded by the scheduler.
///
/// Updated as requests complete: `in_flight` increments on dispatch
/// and decrements on completion; latency and failure metrics roll
/// over a recent window (typical: last 50 requests). `samples` is
/// the total observation count — gates cold-start ramping and
/// observation-vs-claim confidence blending.
///
/// Observations are **local** to each scheduler per §7 — they are
/// never advertised between nodes.
#[derive(Debug, Clone, Default)]
pub struct NodeObservations {
    /// Currently outstanding requests on this node.
    pub in_flight: u32,
    /// Median observed latency over the recent window, in ms.
    pub p50_latency_ms: u32,
    /// 95th-percentile observed latency in ms — catches slow-path
    /// behaviour the p50 hides.
    pub p95_latency_ms: u32,
    /// Fraction of recent requests that failed (0.0 = clean,
    /// 1.0 = every recent request failed). The scheduler uses this
    /// as the primary "observed health" signal.
    pub recent_failure_rate: f32,
    /// Total observed requests this scheduler has recorded for the
    /// node. Used by [`effective_affinity`] to weight claim vs.
    /// observation, and by [`cold_start_weight`] to ramp new
    /// peers in gradually.
    pub samples: u32,
    /// EWMA (α=0.3) of time-to-first-token in milliseconds. Captures
    /// dispatch + first-token latency, the human-perceived "did it
    /// hear me" signal. Not directly used in throughput scoring but
    /// surfaced to operators in diagnostics and the desktop members
    /// panel. Zero until at least one streaming request has completed.
    pub ttft_ewma_ms: f64,
    /// EWMA (α=0.3) of observed token-generation rate in tokens per
    /// second. Source of truth for [`throughput_factor`] when at
    /// least [`THROUGHPUT_OBSERVATION_THRESHOLD`] samples have
    /// accumulated; below the threshold the scheduler falls back to
    /// the benchmark estimate. Zero before any streaming request has
    /// completed.
    pub tg_tok_s_ewma: f64,
}

/// Sample-count threshold above which observed token-generation rate
/// becomes the source of truth for [`throughput_factor`]. Below this
/// the benchmark estimate is used (or neutral 1.0 if neither is
/// present). Same magnitude as [`COLD_START_SAMPLES`] so a peer that
/// has earned full cold-start weight has also earned its observed
/// throughput signal.
pub const THROUGHPUT_OBSERVATION_THRESHOLD: u32 = 5;

/// Smoothing factor for the throughput / TTFT EWMAs.
/// Matches the latency-probe α at
/// `commonwealth-discovery::latency_probe`. Surfaces thermal
/// throttling within ~3–4 requests; lower α values would make the
/// signal sluggish, higher would make it jittery.
pub const THROUGHPUT_EWMA_ALPHA: f64 = 0.3;

/// Blend a claim's self-reported `affinity` with observed node
/// health.
///
/// - Zero samples → return `claimed` verbatim (trust the advertiser).
/// - Above [`CONFIDENCE_SAMPLES`] → claimed × observed health.
/// - In between: linear ramp weighted by sample count.
///
/// "Observed health" here is `1.0 - recent_failure_rate` — a node
/// with 20% recent failures has health 0.8. Latency-based health is
/// applied separately as part of the load-penalty path so the two
/// factors compound multiplicatively, not additively.
pub fn effective_affinity(claimed: f32, obs: &NodeObservations) -> f32 {
    let claim = if claimed.is_nan() {
        0.0
    } else {
        claimed.clamp(0.0, 1.0)
    };
    if obs.samples == 0 {
        return claim;
    }
    let obs_weight = (obs.samples as f32 / CONFIDENCE_SAMPLES as f32).min(1.0);
    let failure = obs.recent_failure_rate.clamp(0.0, 1.0);
    // Interpolation: claim → claim × (1 - failure) as weight → 1.0.
    claim * (1.0 - obs_weight * failure)
}

/// Multiplicative load penalty applied to a node's score. In
/// `(0.0, 1.0]` — `1.0` at zero in-flight, decreasing with load.
///
/// The curve is hyperbolic (`1 / (1 + k * n)`) rather than linear so
/// the first few in-flight requests barely penalize but the tail
/// diverges past `~1/k`. At `LOAD_COEFFICIENT = 0.05`, 10 in-flight
/// ≈ 0.67 and 20 in-flight ≈ 0.50 — enough to divert a second burst
/// without starving the popular node entirely.
pub fn load_penalty(obs: &NodeObservations) -> f32 {
    let k = LOAD_COEFFICIENT;
    let n = obs.in_flight as f32;
    1.0 / (1.0 + k * n)
}

/// Locality bonus in `[1.0, 1.15]`. Multiplicative — applied to the
/// ranked score so a local 0.7-affinity node can out-rank a remote
/// 0.8-affinity node (0.7 × 1.15 = 0.805 > 0.8 × 1.0).
pub fn locality_bonus(locality: NodeLocality) -> f32 {
    match locality {
        NodeLocality::Local => LOCALITY_LOCAL_BONUS,
        NodeLocality::Near => LOCALITY_NEAR_BONUS,
        NodeLocality::Far => LOCALITY_FAR_BONUS,
    }
}

/// Cold-start ramp weight in `[COLD_START_MIN_WEIGHT, 1.0]`. A node
/// with zero samples starts at [`COLD_START_MIN_WEIGHT`] and ramps
/// linearly to `1.0` over [`COLD_START_SAMPLES`] observations — so
/// new peers still receive routable traffic (otherwise they'd never
/// accumulate history) but don't win a burst until they've proven
/// reliable.
pub fn cold_start_weight(samples: u32) -> f32 {
    if samples >= COLD_START_SAMPLES {
        return 1.0;
    }
    let progress = samples as f32 / COLD_START_SAMPLES as f32;
    COLD_START_MIN_WEIGHT + (1.0 - COLD_START_MIN_WEIGHT) * progress
}

/// A node's measured baseline-model throughput. Recorded once at
/// daemon launch (and re-recorded when [`HardwareProfile`] changes)
/// and gossiped via [`NodeCapabilities.benchmark`]. Lets remote
/// schedulers estimate how a *different* model on the same hardware
/// would perform without running it themselves.
///
/// Wire-tolerant: every field has a serde default so an older peer's
/// `NodeCapabilities` payload (sans benchmark) deserializes cleanly
/// and the resulting `Option<BenchmarkResult>` reads as `None`.
///
/// Surfaced to `tracing=debug` via the `bench: completed` event in
/// the daemon startup path so an operator can verify the benchmark
/// ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkResult {
    /// File-stem of the model that was benchmarked (e.g.
    /// `"bonsai-8b-q1_0"`). Schedulers use this as an opaque token
    /// for cache-invalidation only — they do not parse it.
    pub baseline_model_id: String,
    /// On-disk size in GB of the benchmarked model. The same number
    /// `ProviderModel` advertises for the same model. Schedulers
    /// scale `tg_tok_s` by `baseline_size_gb / candidate_size_gb`
    /// when estimating throughput for a *different* model on this
    /// hardware.
    pub baseline_size_gb: f32,
    /// Prompt-processing throughput in tokens per second over a
    /// standardized prompt.
    pub pp_tok_s: f32,
    /// Token-generation throughput in tokens per second over a
    /// standardized prompt.
    pub tg_tok_s: f32,
    /// Unix seconds the benchmark was measured. Operators use this
    /// to spot a stale benchmark after hardware changes; schedulers
    /// don't gate on it.
    pub measured_at: u64,
}

/// Map an observed token-generation rate (or a benchmark-derived
/// estimate) to a routing multiplier in
/// `[THROUGHPUT_FLOOR, 1.0]`.
///
/// Source-of-truth ordering (spec §3.3):
///
/// 1. **Observed**: at least [`THROUGHPUT_OBSERVATION_THRESHOLD`]
///    samples accumulated → use `obs.tg_tok_s_ewma`.
/// 2. **Benchmark estimate**: the node has a [`BenchmarkResult`] →
///    scale baseline `tg_tok_s` by `baseline_size_gb /
///    candidate_size_gb` (smaller models on the same hardware run
///    faster; larger models run slower).
/// 3. **Neutral**: neither signal exists → return `1.0`.
///
/// Returning `1.0` for a zero-data peer is intentional — slotting
/// the multiplier at the end of the composition chain means a peer
/// with no benchmark and no observations behaves identically to the
/// pre-throughput scoring world. This keeps the change wire-tolerant
/// AND behaviour-tolerant: older peers and brand-new peers don't
/// suddenly drop in score.
pub fn throughput_factor(
    obs: &NodeObservations,
    candidate_size_gb: f32,
    baseline_benchmark: Option<&BenchmarkResult>,
) -> f32 {
    let observed_tg_tok_s =
        if obs.samples >= THROUGHPUT_OBSERVATION_THRESHOLD && obs.tg_tok_s_ewma > 0.0 {
            Some(obs.tg_tok_s_ewma as f32)
        } else {
            None
        };

    let estimated_tg_tok_s = match (observed_tg_tok_s, baseline_benchmark) {
        (Some(rate), _) => rate,
        (None, Some(bench)) => {
            // Smaller models on the same hardware run faster. We
            // scale linearly with model-size ratio, which is the
            // simplest defensible heuristic without running an
            // actual benchmark for the candidate. Real-world scaling
            // is sub-linear (memory bandwidth dominates) but linear
            // is good enough for *ranking*: it preserves order across
            // candidate sizes.
            let ratio = if candidate_size_gb > 0.0 {
                bench.baseline_size_gb / candidate_size_gb
            } else {
                1.0
            };
            (bench.tg_tok_s * ratio).max(0.0)
        }
        (None, None) => return 1.0,
    };

    (estimated_tg_tok_s / THROUGHPUT_REFERENCE_TG_TOK_S).clamp(THROUGHPUT_FLOOR, 1.0)
}

/// String label for a [`throughput_factor`] decision — `"observed"`,
/// `"benchmark_estimate"`, or `"neutral"`. Pure helper for the
/// `oicp_select: throughput_factor` glassbox tracing event so
/// operators see *why* a given factor was chosen, not just the
/// number.
pub fn throughput_factor_source(
    obs: &NodeObservations,
    baseline_benchmark: Option<&BenchmarkResult>,
) -> &'static str {
    if obs.samples >= THROUGHPUT_OBSERVATION_THRESHOLD && obs.tg_tok_s_ewma > 0.0 {
        "observed"
    } else if baseline_benchmark.is_some() {
        "benchmark_estimate"
    } else {
        "neutral"
    }
}

// -----------------------------------------------------------------
// v0.3 §6/§7 — the composed scorer (single source of truth)
//
// 2026-06-10 rationalization: the product below used to be
// implemented three times (sovereign-mesh `adjust_for_observations`,
// sovereign-inference `selector.rs` inline, and a dead commonwealth
// scheduler copy) and had already diverged about the availability
// term. It lives HERE, once, next to its factor helpers; consumers
// log the returned [`ScoreBreakdown`] so every routing decision is
// reconstructible from a single trace event.

/// Score-floor below which score-ties are considered "the same".
/// Floating-point noise in the claim scorer (division-by-max-level
/// produces 1/3, 2/3, 1.0 type values) shouldn't cause spurious
/// decisions where a 5.5 GB model beats a 16.5 GB model by a
/// rounding blip.
pub const SCORING_EPSILON: f32 = 1e-3;

/// A scored model pick from a single manifest: the claim score
/// (protocol-level) alongside the claim's self-reported affinity so
/// operational adjustments can compute the observed-health
/// multiplier, plus the tie-break inputs (`size_gb`, `model_id`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredClaim {
    pub score: f32,
    pub size_gb: Option<f32>,
    pub model_id: String,
    /// Self-reported affinity of the claim this score came from.
    pub claim_affinity: f32,
}

/// Compare two [`ScoredClaim`]s under the selection policy and
/// return the winner:
///
/// 1. Strictly higher `score` wins.
/// 2. Scores tied (within [`SCORING_EPSILON`]): smaller known
///    `size_gb` wins.
/// 3. Known size always beats unknown size on a score tie — an
///    annotated manifest entry represents curated data we trust
///    over a silent BYOM default.
/// 4. Full tie: incumbent (`cur`) wins for stability. Callers use
///    this to encode "local wins ties" and "earlier peer wins
///    duplicate-score ties".
pub fn pick_better(cur: ScoredClaim, new: ScoredClaim) -> ScoredClaim {
    if new.score > cur.score + SCORING_EPSILON {
        return new;
    }
    if cur.score > new.score + SCORING_EPSILON {
        return cur;
    }
    match (cur.size_gb, new.size_gb) {
        (Some(c), Some(n)) if n < c => new,
        (None, Some(_)) => new,
        _ => cur,
    }
}

/// Rank each (model, claim) pair in `manifest` against the request
/// and return the best [`ScoredClaim`] via v0.3 claim-based scoring.
/// Returns `None` when no claim can serve the request. Tie-break per
/// [`pick_better`]. Models advertising `status.available == false`
/// are skipped — they exist in the manifest for inventory, not for
/// routing. (Unification note, 2026-06-10: of the pre-SSOT copies,
/// sovereign-inference filtered availability and sovereign-mesh
/// didn't; the filter is the correct semantics and now applies to
/// both.)
pub fn best_claim_for_request(
    manifest: &ProviderManifest,
    req: &InferenceRequirements,
) -> Option<ScoredClaim> {
    let mut best: Option<ScoredClaim> = None;
    for model in manifest.models.iter().filter(|m| m.status.available) {
        for claim in &model.claims {
            let Some(score) = score_claim_for_request(claim, req) else {
                continue;
            };
            let cand = ScoredClaim {
                score,
                size_gb: model.size_gb,
                model_id: model.id.clone(),
                claim_affinity: claim.effective_affinity(),
            };
            best = Some(match best {
                None => cand,
                Some(cur) => pick_better(cur, cand),
            });
        }
    }
    best
}

/// Every factor of one composed scoring decision — the glassbox
/// artifact. Consumers emit this whole struct in ONE tracing event
/// per candidate, which is what makes "why did peer A beat peer B"
/// answerable from logs alone.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreBreakdown {
    pub claim_score: f32,
    /// `effective_affinity(claimed, obs) / claimed` — observed
    /// failure rate eroding the self-reported affinity.
    pub observation_mult: f32,
    pub load_penalty: f32,
    pub locality_bonus: f32,
    pub cold_start_weight: f32,
    pub throughput_factor: f32,
    /// Why that throughput factor: "observed" | "benchmark_estimate"
    /// | "neutral".
    pub throughput_source: &'static str,
    /// Gossiped `inference_availability`, clamped to `[0.2, 1.0]`;
    /// `1.0` when the caller had no signal (`None`).
    pub availability: f32,
    /// The product of everything above — the routing score.
    pub final_score: f32,
}

/// THE composed v0.3 operational scorer. `claim_score` comes from
/// [`score_claim_for_request`] / [`best_claim_for_request`];
/// `availability` is the gossiped `inference_availability` when the
/// caller has one (peers), `None` otherwise (e.g. scoring the local
/// node, whose business is already captured by `obs.in_flight`).
pub fn score_with_adjustments(
    claim_score: f32,
    claim_affinity: f32,
    obs: &NodeObservations,
    locality: NodeLocality,
    candidate_size_gb: f32,
    baseline_benchmark: Option<&BenchmarkResult>,
    availability: Option<f32>,
) -> ScoreBreakdown {
    let observation_mult = if claim_affinity > 0.0 {
        effective_affinity(claim_affinity, obs) / claim_affinity
    } else {
        1.0
    };
    let load = load_penalty(obs);
    let loc = locality_bonus(locality);
    let cold = cold_start_weight(obs.samples);
    let throughput = throughput_factor(obs, candidate_size_gb, baseline_benchmark);
    let avail = availability.map(|a| a.clamp(0.2, 1.0)).unwrap_or(1.0);
    let final_score = claim_score * observation_mult * load * loc * cold * throughput * avail;
    ScoreBreakdown {
        claim_score,
        observation_mult,
        load_penalty: load,
        locality_bonus: loc,
        cold_start_weight: cold,
        throughput_factor: throughput,
        throughput_source: throughput_factor_source(obs, baseline_benchmark),
        availability: avail,
        final_score,
    }
}

// -----------------------------------------------------------------
// v0.3 §4.3 — Extension hint usage registry
//
// A passive observer that records which extension hints (`x:*`)
// appear on the wire. The registry is a governance input, not a
// routing input: the scheduler ignores it completely; a separate
// promotion process (v0.3 §4.3) reads the counts + first-seen /
// last-seen timestamps to decide which extensions have accumulated
// enough "measurable use over a meaningful time window" to merit
// promotion to the standardized set.
//
// Standardized hints (`general`, `code`) are ignored — they're
// already in the canonical set and governance has nothing to
// decide. Unknown-bare hints (no `x:` prefix, not standardized)
// are also skipped: those are most likely typos or
// future-standardized strings a newer peer knows about, neither of
// which are governance-track signals.
// -----------------------------------------------------------------

/// Aggregate statistics for a single observed extension hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionStats {
    /// The hint as it appeared on the wire, including the `x:`
    /// prefix. Preserved verbatim so governance output shows the
    /// exact string the community uses.
    pub hint: String,
    /// Count of requests that asked for this hint. High values
    /// indicate consumer demand.
    pub requests_seen: u64,
    /// Count of advertised claims carrying this hint (across all
    /// peers observed by this scheduler). High values indicate
    /// provider adoption.
    pub advertisements_seen: u64,
    /// Unix timestamp (seconds since epoch) when this hint was
    /// first observed by this scheduler. `None` → not yet seen.
    pub first_seen_unix: u64,
    /// Unix timestamp (seconds since epoch) of the most recent
    /// observation. Combined with `first_seen_unix` it gives the
    /// "durability" signal the promotion process needs.
    pub last_seen_unix: u64,
}

/// Passive registry that accumulates [`ExtensionStats`] for every
/// extension hint observed on the wire. Owned by each scheduler
/// (not global): observations are local just like the per-node
/// tracker. Nothing in the scheduler consults this registry —
/// callers expose it via a diagnostic readout for operators and
/// the governance tooling.
///
/// Not thread-safe on its own; wrap in `RwLock` when shared.
#[derive(Debug, Default, Clone)]
pub struct ExtensionRegistry {
    entries: HashMap<String, ExtensionStats>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Observe an extension hint appearing on an outgoing request.
    /// Standardized and unknown-bare hints are silently ignored.
    pub fn observe_request(&mut self, hint: &CapabilityHint, now_unix: u64) {
        self.record(hint, now_unix, |stats| {
            stats.requests_seen = stats.requests_seen.saturating_add(1);
        });
    }

    /// Observe an extension hint appearing on an advertised claim
    /// (i.e., fetched in a peer's `ProviderManifest`).
    pub fn observe_advertisement(&mut self, hint: &CapabilityHint, now_unix: u64) {
        self.record(hint, now_unix, |stats| {
            stats.advertisements_seen = stats.advertisements_seen.saturating_add(1);
        });
    }

    fn record<F: FnOnce(&mut ExtensionStats)>(
        &mut self,
        hint: &CapabilityHint,
        now_unix: u64,
        bump: F,
    ) {
        if !hint.is_extension() {
            return;
        }
        let entry = self
            .entries
            .entry(hint.as_str().to_string())
            .or_insert_with(|| ExtensionStats {
                hint: hint.as_str().to_string(),
                requests_seen: 0,
                advertisements_seen: 0,
                first_seen_unix: now_unix,
                last_seen_unix: now_unix,
            });
        entry.last_seen_unix = now_unix;
        bump(entry);
    }

    /// Snapshot of every tracked hint. Ordering is insertion-order;
    /// callers that want canonical ordering should sort on the
    /// fields they care about (e.g., `requests_seen + advertisements_seen`
    /// for a popularity ranking).
    pub fn stats(&self) -> impl Iterator<Item = &ExtensionStats> {
        self.entries.values()
    }

    /// Look up a single hint by its wire form (with `x:` prefix).
    pub fn get(&self, hint: &str) -> Option<&ExtensionStats> {
        self.entries.get(hint)
    }

    /// Number of distinct extension hints the registry has seen.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Score a candidate claim against a request (§6).
///
/// Applies the protocol-level portion of the full scoring function:
///
/// ```text
/// hint_match × context_fits × output_fits × latency_match × affinity
/// ```
///
/// Returns `None` when the claim fails a hard feasibility gate
/// (context or output capacity exceeded) or fails the hint gate
/// (wrong specialization). Returns `Some(score)` in `[0.0, 1.0]`
/// otherwise.
///
/// Schedulers apply their own locality bonus, load penalty, and
/// observation-adjusted affinity *outside* this function — see
/// [`effective_affinity`], [`load_penalty`], [`locality_bonus`],
/// [`cold_start_weight`].
pub fn score_claim_for_request(
    claim: &CapabilityClaim,
    req: &InferenceRequirements,
) -> Option<f32> {
    // Hard gates first per §6.
    if let Some(context) = req.context_tokens {
        if claim.max_context < context {
            return None;
        }
    }
    if let Some(output) = req.max_output_tokens {
        if claim.max_output < output {
            return None;
        }
    }

    let hint = hint_match_score(&claim.hint, &req.effective_hint());
    if hint == 0.0 {
        return None;
    }

    let latency = latency_match_score(claim.latency_class, req.effective_latency_class());

    Some(hint * latency * claim.effective_affinity())
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_constant_matches_spec() {
        assert_eq!(OICP_VERSION, "0.4.0");
    }

    // ───── v0.4 back-compat + new-surface round-trips ─────────

    #[test]
    fn v03_manifest_json_deserialises_into_v04_with_defaults() {
        // A manifest emitted by a v0.3 host carries none of the v0.4
        // fields. It MUST deserialise cleanly with empty/None defaults
        // (§8) — this is the whole back-compat contract.
        let v03 = r#"{
            "oicp_version": "0.3.0",
            "models": [{
                "id": "qwen3-9b",
                "context_tokens": 16384,
                "status": {"available": true, "loaded": true},
                "claims": []
            }],
            "knowledge": {
                "corpora": [],
                "search_endpoint": "/v1/knowledge/search",
                "embed_model": {
                    "model_id": "qwen3-embedding-0.6b",
                    "dimensions": 1024,
                    "pooling": "last",
                    "normalization": "server"
                }
            }
        }"#;
        let m: ProviderManifest = serde_json::from_str(v03).expect("deserialise v0.3");
        assert!(m.features.is_empty(), "no features on a v0.3 manifest");
        assert!(m.models[0].fingerprint.is_none());
        let k = m.knowledge.as_ref().unwrap();
        assert!(k.ingest.is_none(), "no ingest surface on a v0.3 host");
        assert_eq!(
            k.embed_model.as_ref().unwrap().query_instruction_prefix, "",
            "absent prefix defaults to empty"
        );
    }

    #[test]
    fn empty_v04_manifest_serialises_to_v03_shape() {
        // An empty v0.4 manifest must serialise byte-identically to a
        // v0.3 manifest: none of the new fields appear on the wire when
        // empty (skip_serializing_if). This is what keeps v0.3 clients
        // from ever seeing v0.4 fields.
        let m = ProviderManifest::new(vec![]);
        let v = serde_json::to_value(&m).unwrap();
        assert!(v.get("features").is_none(), "empty features omitted");
        let obj = v.as_object().unwrap();
        // Exactly the v0.3 always-present keys (oicp_version + models);
        // provider/knowledge/federation are None → omitted.
        assert_eq!(obj.len(), 2, "only oicp_version + models on the wire");
    }

    #[test]
    fn embed_model_equality_distinguishes_query_prefix() {
        // The prefix is part of the bit-compat equality (§4): two nodes
        // that differ only in the query prefix are NOT compatible.
        let base = EmbedModelInfo {
            model_id: "qwen3-embedding-0.6b".into(),
            dimensions: 1024,
            pooling: PoolingStrategy::Last,
            normalization: NormalizationStrategy::Server,
            query_instruction_prefix: String::new(),
        };
        let prefixed = EmbedModelInfo {
            query_instruction_prefix: "Represent this query: ".into(),
            ..base.clone()
        };
        assert_ne!(base, prefixed, "prefix difference breaks compatibility");
        assert_eq!(base, base.clone());
    }

    #[test]
    fn features_validity_predicate() {
        assert!(features::is_valid(features::CONSTRAINT_JSON_SCHEMA));
        assert!(features::is_valid(features::INGEST_V1));
        assert!(features::is_valid("x:prose"), "well-formed extension");
        assert!(!features::is_valid("x:"), "empty extension tag is invalid");
        assert!(!features::is_valid("bogus"), "unregistered bare feature");
    }

    #[test]
    fn ingest_phase_terminality() {
        assert!(IngestPhase::Complete.is_terminal());
        assert!(IngestPhase::Failed.is_terminal());
        assert!(!IngestPhase::Embedding.is_terminal());
        assert!(!IngestPhase::Pending.is_terminal());
    }

    #[test]
    fn ingest_dtos_round_trip() {
        let mut params = BTreeMap::new();
        params.insert("year".to_string(), serde_json::json!(2026));
        let req = CorpusInstallRequest {
            corpus_id: "acme-emails".into(),
            parameters: params,
        };
        let back: CorpusInstallRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back.corpus_id, "acme-emails");
        assert_eq!(back.parameters["year"], serde_json::json!(2026));

        // An install request with no parameters omits the map on the wire.
        let bare = CorpusInstallRequest {
            corpus_id: "x".into(),
            parameters: BTreeMap::new(),
        };
        let v = serde_json::to_value(&bare).unwrap();
        assert!(v.get("parameters").is_none(), "empty parameters omitted");

        let prog = CorpusProgressResponse {
            progress: BTreeMap::from([(
                "acme-emails".to_string(),
                CorpusIngestProgress {
                    phase: IngestPhase::Embedding,
                    fraction: Some(0.4),
                    detail: None,
                },
            )]),
        };
        let back: CorpusProgressResponse =
            serde_json::from_str(&serde_json::to_string(&prog).unwrap()).unwrap();
        assert_eq!(back.progress["acme-emails"].phase, IngestPhase::Embedding);
    }

    #[test]
    fn ingest_endpoints_test_endpoint_optional() {
        let e = IngestEndpoints {
            install_endpoint: "/oicp/v1/corpus/install".into(),
            progress_endpoint: "/oicp/v1/corpus/progress".into(),
            test_endpoint: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert!(
            v.get("test_endpoint").is_none(),
            "absent recipe-test endpoint omitted"
        );
    }

    #[test]
    fn knowledge_result_legacy_json_deserialises_with_none_chunk_id() {
        // Older peers (pre reading-surface plumbing) emit
        // KnowledgeResult JSON without the chunk_id / source_doc_id
        // fields. Verify they deserialise cleanly to None so
        // wire-compat is preserved across mixed-version meshes.
        let legacy = r#"{
            "content": "Alyosha Karamazov is a novice",
            "title": "The Brothers Karamazov",
            "corpus_id": "brothers_karamazov",
            "url": null,
            "score": 0.87,
            "metadata": {}
        }"#;
        let parsed: KnowledgeResult = serde_json::from_str(legacy).expect("deserialise");
        assert_eq!(parsed.chunk_id, None);
        assert_eq!(parsed.source_doc_id, None);
        assert_eq!(parsed.corpus_id, "brothers_karamazov");

        // And a forward-compat round-trip preserves both fields.
        let modern = KnowledgeResult {
            content: "passage".into(),
            title: Some("title".into()),
            corpus_id: "bk".into(),
            url: None,
            score: 0.5,
            metadata: Default::default(),
            chunk_id: Some(42),
            source_doc_id: Some("bk-ch01".into()),
        };
        let json = serde_json::to_string(&modern).unwrap();
        let back: KnowledgeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.chunk_id, Some(42));
        assert_eq!(back.source_doc_id.as_deref(), Some("bk-ch01"));
    }

    #[test]
    fn requirements_default_is_local_only() {
        let req = InferenceRequirements::default();
        assert_eq!(req.oicp_version, OICP_VERSION);
        assert_eq!(req.sharding(), ShardingPrivacy::LocalOnly);
        assert_eq!(req.effective_hint(), CapabilityHint::general());
        assert_eq!(req.effective_latency_class(), LatencyClass::Normal);
    }

    #[test]
    fn requirements_builders_compose() {
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Fast)
            .with_context_tokens(16_000)
            .with_max_output_tokens(2_000)
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_request_id("test-req");
        assert_eq!(req.effective_hint(), CapabilityHint::code());
        assert_eq!(req.effective_latency_class(), LatencyClass::Fast);
        assert_eq!(req.context_tokens, Some(16_000));
        assert_eq!(req.max_output_tokens, Some(2_000));
        assert_eq!(req.sharding(), ShardingPrivacy::MeshAllowed);
        assert_eq!(req.request_id.as_deref(), Some("test-req"));
    }

    #[test]
    fn requirements_round_trip_minimal() {
        let req = InferenceRequirements::new();
        let json = serde_json::to_string(&req).unwrap();
        let back: InferenceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(back.oicp_version, OICP_VERSION);
        assert!(back.capability_hint.is_none());
        assert!(back.latency_class.is_none());
    }

    #[test]
    fn requirements_serialize_in_spec_shape() {
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Normal)
            .with_context_tokens(8_000)
            .with_max_output_tokens(1_500)
            .with_sharding(ShardingPrivacy::MeshAllowed);
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["oicp_version"], OICP_VERSION);
        assert_eq!(value["capability_hint"], "code");
        assert_eq!(value["latency_class"], "normal");
        assert_eq!(value["context_tokens"], 8_000);
        assert_eq!(value["max_output_tokens"], 1_500);
        assert_eq!(value["privacy"]["sharding"], "mesh_allowed");
    }

    // ───── CapabilityHint ─────────────────────────────────────

    #[test]
    fn capability_hint_standardized_constructors() {
        let g = CapabilityHint::general();
        assert_eq!(g.as_str(), "general");
        assert!(g.is_standardized());
        assert!(!g.is_extension());

        let c = CapabilityHint::code();
        assert_eq!(c.as_str(), "code");
        assert!(c.is_standardized());
    }

    #[test]
    fn capability_hint_extension_applies_prefix() {
        let h = CapabilityHint::extension("prose").expect("valid tag");
        assert_eq!(h.as_str(), "x:prose");
        assert!(h.is_extension());
        assert!(!h.is_standardized());
    }

    #[test]
    fn capability_hint_extension_rejects_malformed() {
        assert_eq!(
            CapabilityHint::extension("").unwrap_err(),
            InvalidCapabilityHint::Empty
        );
        assert_eq!(
            CapabilityHint::extension("x:prose").unwrap_err(),
            InvalidCapabilityHint::AlreadyPrefixed
        );
        assert_eq!(
            CapabilityHint::extension("a b").unwrap_err(),
            InvalidCapabilityHint::Whitespace
        );
        assert_eq!(
            CapabilityHint::extension("general").unwrap_err(),
            InvalidCapabilityHint::CollidesWithStandardized
        );
        assert_eq!(
            CapabilityHint::extension("code").unwrap_err(),
            InvalidCapabilityHint::CollidesWithStandardized
        );
    }

    #[test]
    fn capability_hint_parse_accepts_standardized_extension_and_future() {
        assert_eq!(
            CapabilityHint::parse("general").unwrap().as_str(),
            "general"
        );
        assert_eq!(CapabilityHint::parse("code").unwrap().as_str(), "code");

        let ext = CapabilityHint::parse("x:biomed").unwrap();
        assert_eq!(ext.as_str(), "x:biomed");
        assert!(ext.is_extension());

        let future = CapabilityHint::parse("math").unwrap();
        assert_eq!(future.as_str(), "math");
        assert!(future.is_unknown_bare());

        assert!(CapabilityHint::parse("").is_err());
        assert!(CapabilityHint::parse("has space").is_err());
    }

    #[test]
    fn capability_hint_serde_round_trips_as_plain_string() {
        let hints = vec![
            CapabilityHint::general(),
            CapabilityHint::code(),
            CapabilityHint::extension("prose").unwrap(),
            CapabilityHint::parse("math_future").unwrap(),
        ];
        for h in hints {
            let json = serde_json::to_string(&h).unwrap();
            assert!(json.starts_with('"') && json.ends_with('"'), "got {json}");
            let back: CapabilityHint = serde_json::from_str(&json).unwrap();
            assert_eq!(back, h);
        }
    }

    #[test]
    fn latency_class_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&LatencyClass::Fast).unwrap(),
            "\"fast\""
        );
        assert_eq!(
            serde_json::to_string(&LatencyClass::Normal).unwrap(),
            "\"normal\""
        );
        assert_eq!(
            serde_json::to_string(&LatencyClass::Extended).unwrap(),
            "\"extended\""
        );
        assert_eq!(LatencyClass::default(), LatencyClass::Normal);
    }

    #[test]
    fn capability_claim_fits_gate() {
        let claim = CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            16_000,
            4_000,
            0.85,
        );
        assert!(claim.fits(10_000, 1_000));
        assert!(claim.fits(16_000, 4_000));
        assert!(!claim.fits(16_001, 1_000));
        assert!(!claim.fits(10_000, 4_001));
    }

    #[test]
    fn capability_claim_effective_affinity_clamps() {
        let mk = |a| {
            CapabilityClaim::new(
                CapabilityHint::general(),
                LatencyClass::Normal,
                4_000,
                1_000,
                a,
            )
        };
        assert!((mk(0.7).effective_affinity() - 0.7).abs() < 1e-6);
        assert_eq!(mk(-0.2).effective_affinity(), 0.0);
        assert_eq!(mk(1.5).effective_affinity(), 1.0);
        assert_eq!(mk(f32::NAN).effective_affinity(), 0.0);
    }

    #[test]
    fn capability_claim_serde_round_trip() {
        let claim = CapabilityClaim::new(
            CapabilityHint::extension("biomed").unwrap(),
            LatencyClass::Extended,
            32_000,
            8_000,
            0.9,
        );
        let json = serde_json::to_string(&claim).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["hint"], "x:biomed");
        assert_eq!(v["latency_class"], "extended");
        assert_eq!(v["max_context"], 32_000);
        assert_eq!(v["max_output"], 8_000);

        let back: CapabilityClaim = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hint, claim.hint);
        assert_eq!(back.latency_class, claim.latency_class);
    }

    // ───── ProviderManifest round-trip ──────────────────────

    #[test]
    fn provider_manifest_round_trip_with_claims() {
        let model = ProviderModel {
            id: "qwen3-9b".into(),
            base_model: None,
            quantization: Some("Q4_K_M".into()),
            context_tokens: 16_384,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: Some(42.0),
                estimated_ttft_ms: Some(900),
                estimated_load_time_sec: None,
            },
            size_gb: Some(5.2),
            claims: vec![
                CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Fast,
                    4_000,
                    500,
                    0.75,
                ),
                CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Normal,
                    16_000,
                    2_000,
                    0.6,
                ),
            ],
            fingerprint: None,
        };
        let manifest = ProviderManifest::new(vec![model]);
        let json = serde_json::to_string(&manifest).unwrap();
        let back: ProviderManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.models[0].claims.len(), 2);
        assert_eq!(back.models[0].claims[0].latency_class, LatencyClass::Fast);
    }

    // ───── Scoring ──────────────────────────────────────────

    fn claim(
        hint: CapabilityHint,
        lc: LatencyClass,
        ctx: u32,
        out: u32,
        aff: f32,
    ) -> CapabilityClaim {
        CapabilityClaim::new(hint, lc, ctx, out, aff)
    }

    fn req_with(
        hint: CapabilityHint,
        lc: LatencyClass,
        ctx: u32,
        out: u32,
    ) -> InferenceRequirements {
        InferenceRequirements::new()
            .with_hint(hint)
            .with_latency_class(lc)
            .with_context_tokens(ctx)
            .with_max_output_tokens(out)
    }

    #[test]
    fn hint_match_exact_is_one() {
        assert_eq!(
            hint_match_score(&CapabilityHint::code(), &CapabilityHint::code()),
            1.0
        );
        assert_eq!(
            hint_match_score(&CapabilityHint::general(), &CapabilityHint::general()),
            1.0
        );
    }

    #[test]
    fn hint_match_general_request_against_specific_claim_is_zero() {
        assert_eq!(
            hint_match_score(&CapabilityHint::code(), &CapabilityHint::general()),
            0.0
        );
        assert_eq!(
            hint_match_score(
                &CapabilityHint::extension("biomed").unwrap(),
                &CapabilityHint::general()
            ),
            0.0
        );
    }

    #[test]
    fn hint_match_specific_request_with_general_claim_is_fallback() {
        assert_eq!(
            hint_match_score(&CapabilityHint::general(), &CapabilityHint::code()),
            HINT_GENERAL_FALLBACK_SCORE
        );
    }

    #[test]
    fn hint_match_specific_vs_different_specific_is_zero() {
        assert_eq!(
            hint_match_score(
                &CapabilityHint::code(),
                &CapabilityHint::extension("prose").unwrap()
            ),
            0.0
        );
    }

    #[test]
    fn latency_match_exact_adjacent_and_gap() {
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Fast),
            1.0
        );
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Normal),
            LATENCY_ADJACENT_SCORE
        );
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Extended),
            LATENCY_TWO_CLASS_SCORE
        );
    }

    #[test]
    fn score_hard_gate_eliminates_insufficient_context() {
        let c = claim(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_000,
            2_000,
            0.9,
        );
        let over = req_with(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_001,
            1_000,
        );
        assert_eq!(score_claim_for_request(&c, &over), None);
    }

    #[test]
    fn score_wrong_specialization_returns_none() {
        let c = claim(
            CapabilityHint::extension("prose").unwrap(),
            LatencyClass::Normal,
            16_000,
            2_000,
            0.9,
        );
        let req = req_with(CapabilityHint::code(), LatencyClass::Normal, 4_000, 1_000);
        assert_eq!(score_claim_for_request(&c, &req), None);
    }

    #[test]
    fn score_full_formula_multiplies_hint_latency_affinity() {
        // code/fast claim against code/fast request: 1.0 × 1.0 × 0.9 = 0.9.
        let c = claim(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.9,
        );
        let req = req_with(CapabilityHint::code(), LatencyClass::Fast, 4_000, 500);
        let score = score_claim_for_request(&c, &req).expect("passes");
        // hint=1.0, latency=Fast vs Normal adjacent=0.8, affinity=0.9
        assert!((score - 0.72).abs() < 1e-6, "got {score}");
    }

    // ───── v0.3 §7 — observation helpers ───────────────────

    fn obs_with(in_flight: u32, failures: f32, samples: u32) -> NodeObservations {
        NodeObservations {
            in_flight,
            p50_latency_ms: 0,
            p95_latency_ms: 0,
            recent_failure_rate: failures,
            samples,
            ttft_ewma_ms: 0.0,
            tg_tok_s_ewma: 0.0,
        }
    }

    #[test]
    fn effective_affinity_trusts_claim_with_zero_samples() {
        let obs = obs_with(0, 0.8, 0); // 80% failure claim — ignored
        assert!(
            (effective_affinity(0.9, &obs) - 0.9).abs() < 1e-6,
            "zero-sample observations must not override the claim"
        );
    }

    #[test]
    fn effective_affinity_fully_applies_observation_past_threshold() {
        let obs = obs_with(0, 0.2, CONFIDENCE_SAMPLES);
        // claim 0.9, failure 0.2 → 0.9 × (1 - 1.0 × 0.2) = 0.72
        let eff = effective_affinity(0.9, &obs);
        assert!((eff - 0.72).abs() < 1e-6, "got {eff}");
    }

    #[test]
    fn effective_affinity_ramps_observation_weight() {
        // At half CONFIDENCE_SAMPLES the observation should weigh 50%.
        let obs = obs_with(0, 0.4, CONFIDENCE_SAMPLES / 2);
        // 0.8 × (1 - 0.5 × 0.4) = 0.8 × 0.8 = 0.64
        let eff = effective_affinity(0.8, &obs);
        assert!((eff - 0.64).abs() < 1e-6, "got {eff}");
    }

    #[test]
    fn effective_affinity_clamps_and_handles_nan() {
        assert_eq!(effective_affinity(1.5, &obs_with(0, 0.0, 0)), 1.0);
        assert_eq!(effective_affinity(-0.2, &obs_with(0, 0.0, 0)), 0.0);
        assert_eq!(effective_affinity(f32::NAN, &obs_with(0, 0.0, 0)), 0.0);
    }

    #[test]
    fn load_penalty_is_one_at_zero_in_flight() {
        assert_eq!(load_penalty(&obs_with(0, 0.0, 0)), 1.0);
    }

    #[test]
    fn load_penalty_decreases_monotonically() {
        let ten = load_penalty(&obs_with(10, 0.0, 0));
        let twenty = load_penalty(&obs_with(20, 0.0, 0));
        let fifty = load_penalty(&obs_with(50, 0.0, 0));
        assert!(ten > twenty);
        assert!(twenty > fifty);
        assert!(
            fifty > 0.0,
            "must never collapse to zero — that would eliminate the node entirely"
        );
    }

    #[test]
    fn load_penalty_curve_hits_documented_points() {
        // Check the spec comment's example points within 10%.
        let ten = load_penalty(&obs_with(10, 0.0, 0));
        assert!((ten - 0.667).abs() < 0.01, "got {ten}");
        let twenty = load_penalty(&obs_with(20, 0.0, 0));
        assert!((twenty - 0.5).abs() < 0.01, "got {twenty}");
    }

    #[test]
    fn locality_bonus_order() {
        assert!(locality_bonus(NodeLocality::Local) > locality_bonus(NodeLocality::Near));
        assert!(locality_bonus(NodeLocality::Near) > locality_bonus(NodeLocality::Far));
        assert_eq!(locality_bonus(NodeLocality::Far), 1.0);
    }

    #[test]
    fn locality_bonus_strength_matches_spec() {
        // A local 0.7-affinity node must beat a remote 0.8-affinity
        // node per the spec's worked example.
        let local = 0.7 * locality_bonus(NodeLocality::Local);
        let far = 0.8 * locality_bonus(NodeLocality::Far);
        assert!(local > far, "local {local} must beat far {far}");
    }

    // ───── v0.3 §4.3 — Extension registry ──────────────────

    #[test]
    fn extension_registry_records_extension_on_first_observation() {
        let mut reg = ExtensionRegistry::new();
        let hint = CapabilityHint::extension("prose").unwrap();
        reg.observe_request(&hint, 1_000);
        let stats = reg.get("x:prose").expect("must be recorded");
        assert_eq!(stats.requests_seen, 1);
        assert_eq!(stats.advertisements_seen, 0);
        assert_eq!(stats.first_seen_unix, 1_000);
        assert_eq!(stats.last_seen_unix, 1_000);
    }

    #[test]
    fn extension_registry_accumulates_counts_and_updates_last_seen() {
        let mut reg = ExtensionRegistry::new();
        let hint = CapabilityHint::extension("prose").unwrap();
        reg.observe_request(&hint, 1_000);
        reg.observe_advertisement(&hint, 1_500);
        reg.observe_request(&hint, 2_000);
        let stats = reg.get("x:prose").unwrap();
        assert_eq!(stats.requests_seen, 2);
        assert_eq!(stats.advertisements_seen, 1);
        // first_seen stays pinned at the earliest observation; last_seen
        // advances monotonically.
        assert_eq!(stats.first_seen_unix, 1_000);
        assert_eq!(stats.last_seen_unix, 2_000);
    }

    #[test]
    fn extension_registry_ignores_standardized_hints() {
        let mut reg = ExtensionRegistry::new();
        reg.observe_request(&CapabilityHint::general(), 1_000);
        reg.observe_request(&CapabilityHint::code(), 2_000);
        reg.observe_advertisement(&CapabilityHint::code(), 3_000);
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn extension_registry_ignores_unknown_bare_hints() {
        // A bare unrecognised string (e.g., "math" before any
        // hypothetical future promotion) is forward-compatibility
        // data, not a governance signal — skip it.
        let mut reg = ExtensionRegistry::new();
        let future = CapabilityHint::parse("math").unwrap();
        assert!(future.is_unknown_bare());
        reg.observe_request(&future, 1_000);
        reg.observe_advertisement(&future, 2_000);
        assert!(reg.is_empty());
    }

    #[test]
    fn extension_registry_tracks_multiple_hints_independently() {
        let mut reg = ExtensionRegistry::new();
        let prose = CapabilityHint::extension("prose").unwrap();
        let biomed = CapabilityHint::extension("biomed").unwrap();
        reg.observe_request(&prose, 1_000);
        reg.observe_advertisement(&biomed, 1_500);
        reg.observe_request(&prose, 2_000);
        assert_eq!(reg.len(), 2);
        let prose_stats = reg.get("x:prose").unwrap();
        assert_eq!(prose_stats.requests_seen, 2);
        assert_eq!(prose_stats.advertisements_seen, 0);
        let biomed_stats = reg.get("x:biomed").unwrap();
        assert_eq!(biomed_stats.requests_seen, 0);
        assert_eq!(biomed_stats.advertisements_seen, 1);
    }

    #[test]
    fn cold_start_ramps_from_min_to_one() {
        assert_eq!(cold_start_weight(0), COLD_START_MIN_WEIGHT);
        assert_eq!(cold_start_weight(COLD_START_SAMPLES), 1.0);
        assert_eq!(cold_start_weight(COLD_START_SAMPLES + 1_000), 1.0);
        // Monotonic between 0 and the threshold.
        let mid = cold_start_weight(COLD_START_SAMPLES / 2);
        assert!(mid > COLD_START_MIN_WEIGHT && mid < 1.0, "got {mid}");
    }

    // ───── v0.3 §3 — throughput scoring ────────────────────

    fn obs_with_throughput(samples: u32, tg: f64) -> NodeObservations {
        NodeObservations {
            in_flight: 0,
            p50_latency_ms: 0,
            p95_latency_ms: 0,
            recent_failure_rate: 0.0,
            samples,
            ttft_ewma_ms: 0.0,
            tg_tok_s_ewma: tg,
        }
    }

    fn benchmark(baseline_size_gb: f32, tg: f32) -> BenchmarkResult {
        BenchmarkResult {
            baseline_model_id: "bonsai-8b-q1_0".into(),
            baseline_size_gb,
            pp_tok_s: 100.0,
            tg_tok_s: tg,
            measured_at: 1_700_000_000,
        }
    }

    #[test]
    fn throughput_factor_neutral_without_data() {
        let obs = obs_with_throughput(0, 0.0);
        assert_eq!(
            throughput_factor(&obs, 8.0, None),
            1.0,
            "no observations + no benchmark must be neutral 1.0"
        );
        assert_eq!(throughput_factor_source(&obs, None), "neutral");
    }

    #[test]
    fn throughput_factor_floor_at_low_observed_rate() {
        let obs = obs_with_throughput(100, 3.0);
        assert!(
            (throughput_factor(&obs, 8.0, None) - THROUGHPUT_FLOOR).abs() < 1e-6,
            "3 tok/s observed must clamp to floor"
        );
        assert_eq!(throughput_factor_source(&obs, None), "observed");
    }

    #[test]
    fn throughput_factor_one_at_or_above_reference() {
        let obs = obs_with_throughput(100, 25.0);
        assert_eq!(
            throughput_factor(&obs, 8.0, None),
            1.0,
            ">= reference rate must produce 1.0"
        );
    }

    #[test]
    fn throughput_factor_scales_linearly_in_band() {
        // 10 tok/s observed → 10/20 = 0.5
        let obs = obs_with_throughput(100, 10.0);
        let f = throughput_factor(&obs, 8.0, None);
        assert!((f - 0.5).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn throughput_factor_falls_back_to_benchmark_estimate_below_threshold() {
        // Below sample threshold → ignore observation, use benchmark.
        let obs = obs_with_throughput(2, 100.0); // huge observation but ignored
        let bench = benchmark(8.0, 20.0);
        // Same model size: ratio 1.0, estimated tg = 20 → factor 1.0.
        let f = throughput_factor(&obs, 8.0, Some(&bench));
        assert!((f - 1.0).abs() < 1e-6, "got {f}");
        assert_eq!(
            throughput_factor_source(&obs, Some(&bench)),
            "benchmark_estimate"
        );
    }

    #[test]
    fn throughput_factor_extrapolates_by_size_ratio() {
        // Baseline 8GB at 20 tok/s. Candidate 16GB → expected ~10 tok/s.
        let bench = benchmark(8.0, 20.0);
        let obs = obs_with_throughput(0, 0.0);
        let f = throughput_factor(&obs, 16.0, Some(&bench));
        // 10/20 = 0.5
        assert!((f - 0.5).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn throughput_factor_observed_overrides_benchmark() {
        // Past threshold, observed wins even when benchmark exists.
        let obs = obs_with_throughput(100, 25.0); // saturates to 1.0
        let bench = benchmark(8.0, 5.0); // would estimate 0.3
        let f = throughput_factor(&obs, 8.0, Some(&bench));
        assert_eq!(f, 1.0);
    }

    #[test]
    fn throughput_factor_zero_size_is_safe() {
        // Defensive: a candidate with size_gb==0 must not divide-by-zero.
        let bench = benchmark(8.0, 20.0);
        let obs = obs_with_throughput(0, 0.0);
        let f = throughput_factor(&obs, 0.0, Some(&bench));
        // ratio defaults to 1.0; estimated rate = 20 → factor 1.0.
        assert!((f - 1.0).abs() < 1e-6, "got {f}");
    }

    #[test]
    fn benchmark_result_is_serde_round_trip() {
        let b = benchmark(8.0, 17.5);
        let json = serde_json::to_string(&b).unwrap();
        let back: BenchmarkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn local_slow_peer_loses_to_remote_fast_peer_after_throughput() {
        // Spec §3.3 composition stability: a local 0.72-affinity peer
        // running at 3 tok/s must lose to a remote 0.78-affinity peer
        // running at 25 tok/s, even after the locality bonus is
        // applied. This pins that throughput_factor dominates the
        // composition when one peer is genuinely slow.
        let local_obs = obs_with_throughput(100, 3.0);
        let remote_obs = obs_with_throughput(100, 25.0);
        let local_score = 0.72_f32
            * locality_bonus(NodeLocality::Local)
            * throughput_factor(&local_obs, 8.0, None);
        let remote_score = 0.78_f32
            * locality_bonus(NodeLocality::Far)
            * throughput_factor(&remote_obs, 8.0, None);
        assert!(
            remote_score > local_score,
            "remote fast {remote_score} must beat local slow {local_score}"
        );
    }

    #[test]
    fn score_coder_collective_ranks_specialist_above_generalist() {
        let qwen_coder = claim(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.95,
        );
        let llama_70b = claim(
            CapabilityHint::general(),
            LatencyClass::Normal,
            64_000,
            4_000,
            0.85,
        );
        let req = req_with(CapabilityHint::code(), LatencyClass::Normal, 16_000, 2_000);
        let a = score_claim_for_request(&qwen_coder, &req).unwrap();
        let b = score_claim_for_request(&llama_70b, &req).unwrap();
        assert!(a > b, "coder {a} must beat general {b}");
        assert!((a - 0.95).abs() < 1e-6);
        // general fallback: 0.5 × 1.0 × 0.85 = 0.425.
        assert!((b - 0.425).abs() < 1e-6);
    }

    // ── score_with_adjustments — the composed SSOT scorer ────────
    //
    // The first block pins the full product (mirrors the golden
    // vector in sovereign-mesh's oicp_select tests, which pinned the
    // pre-SSOT implementation). The scenario tests re-pin the nine
    // behavioral scenarios from the deleted
    // commonwealth-inference/tests/oicp_v03_observations.rs against
    // the SSOT fn directly.

    fn quiet_obs() -> NodeObservations {
        NodeObservations {
            samples: 100, // fully ramped, no cold-start penalty
            ..Default::default()
        }
    }

    fn score(obs: &NodeObservations, locality: NodeLocality, avail: Option<f32>) -> f32 {
        score_with_adjustments(0.8, 0.9, obs, locality, 8.0, None, avail).final_score
    }

    #[test]
    fn composed_product_all_factors_active_golden() {
        let obs = NodeObservations {
            in_flight: 10,
            samples: 10,
            recent_failure_rate: 0.1,
            tg_tok_s_ewma: 10.0,
            ..Default::default()
        };
        let b = score_with_adjustments(0.5, 0.95, &obs, NodeLocality::Near, 8.0, None, None);
        assert!((b.observation_mult - 0.98).abs() < 1e-6);
        assert!((b.load_penalty - 2.0 / 3.0).abs() < 1e-6);
        assert!((b.locality_bonus - 1.05).abs() < 1e-6);
        assert!((b.cold_start_weight - 0.85).abs() < 1e-6);
        assert!((b.throughput_factor - 0.5).abs() < 1e-6);
        assert_eq!(b.throughput_source, "observed");
        assert!((b.availability - 1.0).abs() < 1e-6, "None ⇒ neutral 1.0");
        let expected = 0.5_f32 * 0.98 * (2.0 / 3.0) * 1.05 * 0.85 * 0.5;
        assert!((b.final_score - expected).abs() < 1e-6);
    }

    #[test]
    fn availability_none_is_bit_identical_to_pre_adoption_product() {
        // The adoption contract: availability=None reproduces the old
        // (term-free) formula exactly — same product, no epsilon.
        let obs = NodeObservations {
            in_flight: 3,
            samples: 30,
            recent_failure_rate: 0.05,
            tg_tok_s_ewma: 18.0,
            ..Default::default()
        };
        let without = score_with_adjustments(0.7, 0.85, &obs, NodeLocality::Far, 4.0, None, None);
        let manual = 0.7
            * (effective_affinity(0.85, &obs) / 0.85)
            * load_penalty(&obs)
            * locality_bonus(NodeLocality::Far)
            * cold_start_weight(obs.samples)
            * throughput_factor(&obs, 4.0, None);
        assert_eq!(without.final_score.to_bits(), manual.to_bits());
    }

    #[test]
    fn availability_clamps_floor_and_ceiling() {
        let obs = quiet_obs();
        let floor = score_with_adjustments(0.8, 0.9, &obs, NodeLocality::Far, 8.0, None, Some(0.0));
        assert!(
            (floor.availability - 0.2).abs() < 1e-6,
            "floor 0.2 keeps a busy peer routable"
        );
        let ceil = score_with_adjustments(0.8, 0.9, &obs, NodeLocality::Far, 8.0, None, Some(2.0));
        assert!((ceil.availability - 1.0).abs() < 1e-6);
    }

    #[test]
    fn busy_peer_loses_to_idle_equal_peer_via_availability() {
        // The decided behavior change (2026-06-10): the gossiped
        // availability signal now affects routing. Equal peers,
        // availability 0.2 vs 1.0 — idle wins.
        let obs = quiet_obs();
        let busy = score(&obs, NodeLocality::Far, Some(0.2));
        let idle = score(&obs, NodeLocality::Far, Some(1.0));
        assert!(idle > busy * 4.9, "0.2 vs 1.0 is a 5× score gap");
    }

    // ── re-pinned oicp_v03_observations scenarios ────────────────

    #[test]
    fn thundering_herd_shifts_traffic_to_idle_peer() {
        let mut herd = quiet_obs();
        herd.in_flight = 20; // load_penalty 0.5
        let idle = quiet_obs();
        assert!(score(&idle, NodeLocality::Far, None) > score(&herd, NodeLocality::Far, None));
    }

    #[test]
    fn low_load_keeps_traffic_on_specialist() {
        // A specialist (higher claim score) under LIGHT load still
        // beats an idle generalist: 2 in-flight ⇒ penalty ~0.91.
        let mut light = quiet_obs();
        light.in_flight = 2;
        let specialist =
            score_with_adjustments(1.0, 1.0, &light, NodeLocality::Far, 8.0, None, None);
        let generalist =
            score_with_adjustments(0.5, 0.85, &quiet_obs(), NodeLocality::Far, 8.0, None, None);
        assert!(specialist.final_score > generalist.final_score);
    }

    #[test]
    fn failing_node_loses_to_reliable_peer() {
        let mut flaky = quiet_obs();
        flaky.recent_failure_rate = 0.5; // past ramp ⇒ halves affinity
        assert!(
            score(&quiet_obs(), NodeLocality::Far, None) > score(&flaky, NodeLocality::Far, None)
        );
    }

    #[test]
    fn cold_start_deprioritizes_new_peer_vs_proven_peer() {
        let newcomer = NodeObservations::default(); // samples 0 ⇒ 0.7×
        assert!(
            score(&quiet_obs(), NodeLocality::Far, None)
                > score(&newcomer, NodeLocality::Far, None)
        );
    }

    #[test]
    fn cold_start_fully_ramped_after_threshold_samples() {
        let mut ramped = NodeObservations::default();
        ramped.samples = COLD_START_SAMPLES;
        let b = score_with_adjustments(0.8, 0.9, &ramped, NodeLocality::Far, 8.0, None, None);
        assert!((b.cold_start_weight - 1.0).abs() < 1e-6);
    }

    #[test]
    fn local_node_wins_over_remote_with_higher_affinity() {
        // Locality 1.15 vs 1.0 outweighs a modest claim-score edge:
        // 0.78·1.15 > 0.8·1.0.
        let local = score_with_adjustments(
            0.78,
            0.9,
            &quiet_obs(),
            NodeLocality::Local,
            8.0,
            None,
            None,
        );
        let remote =
            score_with_adjustments(0.8, 0.95, &quiet_obs(), NodeLocality::Far, 8.0, None, None);
        assert!(local.final_score > remote.final_score);
    }

    #[test]
    fn near_lan_peer_beats_far_internet_peer_at_equal_affinity() {
        assert!(
            score(&quiet_obs(), NodeLocality::Near, None)
                > score(&quiet_obs(), NodeLocality::Far, None)
        );
    }

    #[test]
    fn slow_peer_loses_to_fast_peer_under_throughput_scoring() {
        let mut slow = quiet_obs();
        slow.tg_tok_s_ewma = 4.0; // 4/20 ⇒ clamps to floor 0.3
        let mut fast = quiet_obs();
        fast.tg_tok_s_ewma = 30.0; // ≥ reference ⇒ 1.0
        assert!(score(&fast, NodeLocality::Far, None) > score(&slow, NodeLocality::Far, None));
    }

    #[test]
    fn neutral_throughput_preserves_pre_throughput_routing_behavior() {
        // No observed throughput and no benchmark ⇒ factor 1.0 and
        // the decision reduces to the other factors.
        let b = score_with_adjustments(0.8, 0.9, &quiet_obs(), NodeLocality::Far, 8.0, None, None);
        assert!((b.throughput_factor - 1.0).abs() < 1e-6);
        assert_eq!(b.throughput_source, "neutral");
    }

    // ── best_claim_for_request / pick_better ─────────────────────

    #[test]
    fn pick_better_smaller_size_wins_score_tie() {
        let big = ScoredClaim {
            score: 0.8,
            size_gb: Some(16.0),
            model_id: "big".into(),
            claim_affinity: 0.8,
        };
        let small = ScoredClaim {
            score: 0.8,
            size_gb: Some(5.0),
            model_id: "small".into(),
            claim_affinity: 0.8,
        };
        assert_eq!(pick_better(big, small).model_id, "small");
    }

    fn manifest_model(
        id: &str,
        size_gb: f32,
        hint: CapabilityHint,
        affinity: f32,
    ) -> ProviderModel {
        ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: Some(size_gb),
            claims: vec![CapabilityClaim::new(
                hint,
                LatencyClass::Normal,
                32_768,
                4_000,
                affinity,
            )],
            fingerprint: None,
        }
    }

    #[test]
    fn best_claim_for_request_picks_highest_scoring_model() {
        let manifest = ProviderManifest {
            oicp_version: OICP_VERSION.to_string(),
            provider: None,
            models: vec![
                manifest_model("generalist", 16.0, CapabilityHint::general(), 0.85),
                manifest_model("coder", 8.0, CapabilityHint::code(), 0.95),
            ],
            knowledge: None,
            federation: None,
            features: Vec::new(),
        };
        let req = InferenceRequirements {
            oicp_version: OICP_VERSION.to_string(),
            capability_hint: Some(CapabilityHint::code()),
            latency_class: Some(LatencyClass::Normal),
            context_tokens: Some(8_000),
            max_output_tokens: Some(1_000),
            privacy: None,
            request_id: None,
        };
        let best = best_claim_for_request(&manifest, &req).unwrap();
        // Specialist at exact-hint 0.95 beats generalist's 0.5-fallback path.
        assert_eq!(best.model_id, "coder");
    }

    #[test]
    fn knowledge_search_thin_client_shape_deserializes() {
        // OICP v0.4 §6.1: a thin client sends only `query` — no embedding,
        // and the OICP field name `query` (not `query_text`).
        let req: KnowledgeSearchRequest =
            serde_json::from_value(serde_json::json!({"query": "stoic virtue", "limit": 3})).unwrap();
        assert_eq!(req.query_text, "stoic virtue");
        assert!(req.query_embedding.is_empty(), "host embeds when absent");
        assert_eq!(req.effective_limit(), 3);
    }

    #[test]
    fn knowledge_search_mesh_shape_still_deserializes() {
        // The mesh-internal shape (pre-embedded, `query_text`) is unchanged.
        let req: KnowledgeSearchRequest = serde_json::from_value(serde_json::json!({
            "query_embedding": [0.1, 0.2, 0.3],
            "query_text": "stoic virtue",
        }))
        .unwrap();
        assert_eq!(req.query_embedding, vec![0.1, 0.2, 0.3]);
        assert_eq!(req.query_text, "stoic virtue");
    }

    #[test]
    fn knowledge_search_empty_embedding_omitted_from_wire() {
        // An absent embedding must not serialize as `query_embedding: []`.
        let req = KnowledgeSearchRequest {
            query_embedding: Vec::new(),
            query_text: "q".into(),
            corpora: None,
            limit: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("query_embedding").is_none());
    }
}
