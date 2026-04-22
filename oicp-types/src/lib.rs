//! OICP — Open Inference Capabilities Protocol v0.3.0
//!
//! Canonical types per the specifications at
//! `commonwealth/docs/oicp-v0.2.md` (§1–§6, client requirements + provider
//! manifest + response metadata + knowledge search) and
//! `commonwealth/docs/oicp-v0.3.md` (additive specialization-aware routing:
//! capability hints, latency classes, per-model claims).
//!
//! This crate is the single source of truth for all OICP type definitions,
//! consumed by both the Sovereign (`lcol-llm`) and Commonwealth workspaces
//! via path dependency.
//!
//! v0.3 is purely additive. Existing v0.2 types (`Capability`,
//! `CapabilityProfile`, `InferenceRequirements.capabilities`,
//! `ProviderModel.capabilities`) remain unchanged; the new fields
//! (`CapabilityHint`, `LatencyClass`, `CapabilityClaim`,
//! `InferenceRequirements.capability_hint`/`latency_class`/`context_tokens`/
//! `max_output_tokens`, `ProviderModel.claims`) are all optional and
//! default-omitted so v0.2 clients continue to interoperate with reduced
//! routing precision per §10.3.
//!
//! Unqualified section references refer to oicp-v0.2.md. References of the
//! form "v0.3 §N" refer to oicp-v0.3.md.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// OICP specification version implemented by this module.
pub const OICP_VERSION: &str = "0.3.0";

// -----------------------------------------------------------------
// Section 2 — Capability Vocabulary
// -----------------------------------------------------------------

/// Capability domains (§2.1). Per §2.4, unrecognized capability IDs MUST
/// be ignored, not rejected — they deserialize to `Unknown` and are
/// ignored by the helper scoring functions.
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

/// Proficiency level on a 0–4 ordinal scale (§2.2).
/// 0 = None, 1 = Basic, 2 = Moderate, 3 = Strong, 4 = Exceptional.
pub type ProficiencyLevel = u8;

/// A map from capability IDs to proficiency levels (§2.3).
/// Capabilities not present are implicitly level 0.
pub type CapabilityProfile = HashMap<Capability, ProficiencyLevel>;

// -----------------------------------------------------------------
// v0.3 §4 — Capability Hints
// -----------------------------------------------------------------

/// A capability hint naming a kind of inference work (v0.3 §4).
///
/// Two hints are standardized at protocol launch: `general` and `code`.
/// Any other specialization (prose, biomedical, math, dialogue, …) starts
/// as an extension hint, which must carry the `x:` prefix so it is
/// distinguishable at parse time from a standardized hint.
///
/// # Forward compatibility
///
/// Parsing is deliberately permissive: any non-empty, whitespace-free
/// string is accepted. A bare hint that isn't currently standardized in
/// this build (e.g., because a future spec promoted `math` to
/// standardized and this client predates the bump) is preserved
/// verbatim. The scheduler may still match it by exact string against
/// advertised claims; where no match exists it falls back to `general`
/// per v0.3 §4.2. This is the graceful-degradation path required by
/// v0.3 §10.3.
///
/// # Construction
///
/// Use [`CapabilityHint::general`] and [`CapabilityHint::code`] for the
/// standardized constants, [`CapabilityHint::extension`] to wrap an
/// extension tag (the `x:` prefix is applied automatically), and
/// [`CapabilityHint::parse`] to validate a raw string received over the
/// wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityHint(String);

impl CapabilityHint {
    /// Standardized hint: no specific specialization target. Every node
    /// serving inference supports this hint; every request without a
    /// more specific hint uses this (v0.3 §4.1).
    pub const GENERAL: &'static str = "general";

    /// Standardized hint: code generation, understanding, modification,
    /// review. Models tuned for code emphasis serve this with higher
    /// affinity than general-purpose models (v0.3 §4.1).
    pub const CODE: &'static str = "code";

    /// Extension prefix: extension hints must start with `x:`
    /// (v0.3 §4.2). Chosen over `ext/` or `@ns/` for brevity and
    /// because `:` never appears in a standardized hint.
    pub const EXTENSION_PREFIX: &'static str = "x:";

    /// Known standardized hints as of this crate build.
    /// Grows by governance decision (v0.3 §4.3); do not extend locally.
    pub const STANDARDIZED: &'static [&'static str] =
        &[Self::GENERAL, Self::CODE];

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
    /// prefix (double-prefix guard), tags that collide with a
    /// standardized hint in this build, and tags containing whitespace.
    pub fn extension(
        tag: impl AsRef<str>,
    ) -> Result<Self, InvalidCapabilityHint> {
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
        if Self::STANDARDIZED.iter().any(|s| *s == tag) {
            return Err(InvalidCapabilityHint::CollidesWithStandardized);
        }
        Ok(Self(format!("{}{}", Self::EXTENSION_PREFIX, tag)))
    }

    /// Parse a raw hint string (e.g., from a manifest or request).
    ///
    /// Accepts any non-empty, whitespace-free string. Rejection is
    /// reserved for structurally unusable values — bare strings that
    /// aren't currently standardized here are preserved verbatim so
    /// forward-compatible matching still works (v0.3 §10.3).
    pub fn parse(
        raw: impl AsRef<str>,
    ) -> Result<Self, InvalidCapabilityHint> {
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

    /// True iff this hint carries the `x:` extension prefix and has a
    /// non-empty tag component.
    pub fn is_extension(&self) -> bool {
        self.0.starts_with(Self::EXTENSION_PREFIX)
            && self.0.len() > Self::EXTENSION_PREFIX.len()
    }

    /// True iff this hint is neither standardized in this build nor an
    /// extension — likely a future-standardized hint. Schedulers should
    /// still attempt exact-string matches against advertised claims
    /// before falling back to `general`.
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
    /// The extension tag equals a standardized hint (`general` or
    /// `code`); use [`CapabilityHint::general`] / [`CapabilityHint::code`]
    /// instead.
    CollidesWithStandardized,
}

impl std::fmt::Display for InvalidCapabilityHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Empty => "capability hint is empty",
            Self::AlreadyPrefixed => {
                "capability hint already carries the 'x:' extension prefix"
            }
            Self::Whitespace => "capability hint contains whitespace",
            Self::CollidesWithStandardized => {
                "extension tag collides with a standardized hint"
            }
        };
        f.write_str(msg)
    }
}

impl std::error::Error for InvalidCapabilityHint {}

impl Serialize for CapabilityHint {
    fn serialize<S: Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CapabilityHint {
    fn deserialize<D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

// -----------------------------------------------------------------
// v0.3 §5 — Latency Classes
// -----------------------------------------------------------------

/// Time-sensitivity class for a request or a node's typical response
/// time for a kind of work (v0.3 §5).
///
/// These are categories, not precise SLAs. A node advertises the class
/// matching its typical behaviour; a request names the class it needs.
/// The scheduler prefers matching classes and treats mismatch as a
/// soft deprioritization rather than a hard failure (v0.3 §6).
///
/// Distinct from [`LatencyPreference`] (v0.2 §3.1), which expresses a
/// client's desired policy (interactive, throughput, best-effort).
/// [`latency_class_from_preference`] translates between the two for
/// backward compatibility.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    /// Time-to-first-token in hundreds of milliseconds. Suitable for
    /// routing, classification, short extractions, interactive UI.
    Fast,
    /// Time-to-first-token in single-digit seconds. Suitable for most
    /// substantive inference work. Default.
    #[default]
    Normal,
    /// TTFT may be longer; total generation may span tens of seconds
    /// or more. Suitable for reasoning-heavy work, long-context
    /// synthesis, deep planning.
    Extended,
}

// -----------------------------------------------------------------
// v0.3 §3.1 — Capability Claims
// -----------------------------------------------------------------

/// A capability claim: what kind of inference work this node serves,
/// with a stated affinity (v0.3 §3.1).
///
/// A node may publish multiple claims — one per (model, latency class)
/// combination, or one per model for nodes running a single model. A
/// claim is the unit of scheduling: the scheduler ranks (node, claim)
/// pairs against a request's property set (v0.3 §6).
///
/// `affinity` is the node's self-assessment of how well it serves this
/// kind of work, clamped to `[0.0, 1.0]` with documented reference
/// points:
///
/// - `1.0` — exceptional fit (specialized model serving its specialty).
/// - `~0.85` — strong fit (large general model serving general work).
/// - `~0.7` — solid fit (small fast model serving fast work well).
/// - `~0.5` — adequate fit (model can serve but prefer elsewhere).
/// - below `0.5` — feasible but substantially degraded.
///
/// Affinity is self-reported and therefore less reliable than
/// structural facts (context / output capacity, hint match). The
/// scheduler treats it as a tiebreaker, not a primary ranker, and
/// cross-checks it against observed performance over time (v0.3 §7.4).
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
    /// Self-assessed affinity in `[0.0, 1.0]`; see type-level docs for
    /// reference points. Accessed via
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

    /// Affinity clamped to `[0.0, 1.0]` per v0.3 §3.1 semantics. NaN
    /// collapses to 0.0.
    pub fn effective_affinity(&self) -> f32 {
        if self.affinity.is_nan() {
            0.0
        } else {
            self.affinity.clamp(0.0, 1.0)
        }
    }

    /// True iff this claim can fit the structural size of a request
    /// (v0.3 §6 — context/output are hard constraints).
    pub fn fits(&self, context_tokens: u32, max_output_tokens: u32) -> bool {
        self.max_context >= context_tokens
            && self.max_output >= max_output_tokens
    }
}

// -----------------------------------------------------------------
// Section 3 — Client Requirements Schema
// -----------------------------------------------------------------

/// What a client needs from an inference call (§3, extended by v0.3 §3.2).
///
/// v0.3 adds four optional property fields (`capability_hint`,
/// `latency_class`, `context_tokens`, `max_output_tokens`) alongside the
/// v0.2 `capabilities`/`context`/`performance` structures. A v0.3
/// scheduler prefers the property fields when both sides carry them and
/// falls back to the v0.2 capability-profile path otherwise (v0.3 §8
/// behavioural requirements).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequirements {
    pub oicp_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CapabilityRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<PerformanceRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<PrivacyRequirements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// v0.3 §3.2: capability hint for specialization-aware routing.
    /// Absent → scheduler treats as `general` per v0.3 §8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_hint: Option<CapabilityHint>,
    /// v0.3 §3.2: latency class the request needs. Absent → scheduler
    /// treats as [`LatencyClass::Normal`] per v0.3 §8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_class: Option<LatencyClass>,
    /// v0.3 §3.2: actual context length of the request. Used by the
    /// scheduler as a hard feasibility gate against each claim's
    /// `max_context` (v0.3 §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// v0.3 §3.2: expected output length. Used by the scheduler as a
    /// hard feasibility gate against each claim's `max_output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl Default for InferenceRequirements {
    fn default() -> Self {
        Self {
            oicp_version: OICP_VERSION.to_string(),
            capabilities: None,
            context: None,
            performance: None,
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

    /// Builder: set the latency preference. Allocates `performance` if absent.
    pub fn with_latency(mut self, latency: LatencyPreference) -> Self {
        self.performance = Some(PerformanceRequirements {
            latency: Some(latency),
        });
        self
    }

    /// Builder: set the sharding privacy. Allocates `privacy` if absent.
    pub fn with_sharding(mut self, sharding: ShardingPrivacy) -> Self {
        self.privacy = Some(PrivacyRequirements { sharding });
        self
    }

    /// Builder: set the capability requirements.
    pub fn with_capabilities(mut self, caps: CapabilityRequirements) -> Self {
        self.capabilities = Some(caps);
        self
    }

    /// Builder: set the context requirements.
    pub fn with_context(mut self, context: ContextRequirements) -> Self {
        self.context = Some(context);
        self
    }

    /// Builder: set the request id.
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// v0.3 builder: set the capability hint.
    pub fn with_hint(mut self, hint: CapabilityHint) -> Self {
        self.capability_hint = Some(hint);
        self
    }

    /// v0.3 builder: set the latency class.
    pub fn with_latency_class(mut self, class: LatencyClass) -> Self {
        self.latency_class = Some(class);
        self
    }

    /// v0.3 builder: set the actual context length.
    pub fn with_context_tokens(mut self, tokens: u32) -> Self {
        self.context_tokens = Some(tokens);
        self
    }

    /// v0.3 builder: set the expected output length.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// v0.3 §8: effective hint, defaulting to `general` when absent.
    pub fn effective_hint(&self) -> CapabilityHint {
        self.capability_hint
            .clone()
            .unwrap_or_else(CapabilityHint::general)
    }

    /// v0.3 §8: effective latency class, defaulting to `Normal`.
    pub fn effective_latency_class(&self) -> LatencyClass {
        self.latency_class.unwrap_or(LatencyClass::Normal)
    }

    /// Effective latency, defaulting to `BestEffort` if unset.
    pub fn latency(&self) -> LatencyPreference {
        self.performance
            .as_ref()
            .and_then(|p| p.latency)
            .unwrap_or_default()
    }

    /// Effective sharding privacy, defaulting to `LocalOnly` per §3.1.
    pub fn sharding(&self) -> ShardingPrivacy {
        self.privacy
            .as_ref()
            .map(|p| p.sharding)
            .unwrap_or_default()
    }

    /// Required capability profile, or an empty borrowed view.
    pub fn required(&self) -> &CapabilityProfile {
        static EMPTY: std::sync::OnceLock<CapabilityProfile> = std::sync::OnceLock::new();
        match self.capabilities.as_ref() {
            Some(c) => &c.required,
            None => EMPTY.get_or_init(CapabilityProfile::new),
        }
    }

    /// Preferred capability profile, or an empty borrowed view.
    pub fn preferred(&self) -> &CapabilityProfile {
        static EMPTY: std::sync::OnceLock<CapabilityProfile> = std::sync::OnceLock::new();
        match self.capabilities.as_ref() {
            Some(c) => &c.preferred,
            None => EMPTY.get_or_init(CapabilityProfile::new),
        }
    }

    /// Minimum context tokens, if specified.
    pub fn min_tokens(&self) -> Option<u32> {
        self.context.as_ref().and_then(|c| c.min_tokens)
    }

    /// Preferred context tokens, if specified.
    pub fn preferred_tokens(&self) -> Option<u32> {
        self.context.as_ref().and_then(|c| c.preferred_tokens)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub required: CapabilityProfile,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub preferred: CapabilityProfile,
}

impl CapabilityRequirements {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.required.is_empty() && self.preferred.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextRequirements {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerformanceRequirements {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<LatencyPreference>,
}

/// Latency preference (§3.1). Default `BestEffort` per the spec.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyPreference {
    Interactive,
    Throughput,
    Background,
    #[default]
    BestEffort,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrivacyRequirements {
    #[serde(default)]
    pub sharding: ShardingPrivacy,
}

/// Whether the provider may distribute inference across multiple nodes (§3.1).
///
/// Default is `LocalOnly`. The spec calls this out explicitly: "privacy is
/// the default, not something the client has to remember to request."
/// Clients that want distributed inference must opt in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardingPrivacy {
    #[default]
    LocalOnly,
    MeshAllowed,
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
}

impl ProviderManifest {
    pub fn new(models: Vec<ProviderModel>) -> Self {
        Self {
            oicp_version: OICP_VERSION.to_string(),
            provider: None,
            models,
            knowledge: None,
            federation: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "type"
    )]
    pub provider_type: Option<ProviderType>,
}

/// Provider type hint (§4.1). Informational only — clients MUST NOT make
/// routing decisions based on this field.
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
    pub capabilities: CapabilityProfile,
    pub context_tokens: u32,
    pub status: ModelStatus,
    /// Approximate on-disk weight size in gigabytes. Used as a
    /// tiebreaker during OICP backend selection: when two models
    /// score equally against a request's preferred profile, prefer
    /// the smaller one (smaller ≈ faster TTFT, lighter memory
    /// footprint, less energy). Not a routing input on its own —
    /// capability satisfaction always comes first. Optional because
    /// providers may not know or want to publish this; absent values
    /// sort after any known value so an unknown-size model never
    /// spuriously wins a tie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_gb: Option<f32>,
    /// v0.3 §4: capability claims advertised for this model. Each
    /// claim describes a (capability hint × latency class × context ×
    /// output × affinity) combination the model serves well. Multiple
    /// claims per model are expected when a single model handles more
    /// than one latency class (e.g., a 9B general model serving both
    /// fast short-context and normal long-context work).
    ///
    /// Empty vector means this provider has not yet produced v0.3
    /// claims — consumers fall back to the v0.2 `capabilities` profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<CapabilityClaim>,
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
// Embed model compatibility (used by collaborative ingestion, §4-ext)
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
/// similarity across different embedding spaces is meaningless).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbedModelInfo {
    /// Model identifier, e.g. `"qwen3-embedding-0.6b"`.
    pub model_id: String,
    /// Output vector dimensionality.
    pub dimensions: usize,
    pub pooling: PoolingStrategy,
    pub normalization: NormalizationStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeManifest {
    pub corpora: Vec<CorpusDescriptor>,
    pub search_endpoint: String,
    /// Embed model in use on this node. `None` means the node has not
    /// advertised its embed configuration — exclude from collaborative
    /// ingestion until this is populated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<EmbedModelInfo>,
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
    pub model_capabilities: Option<CapabilityProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_quality: Option<MatchQuality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_capabilities: Option<HashMap<Capability, DegradedDetail>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchQuality {
    Full,
    Partial,
    Degraded,
    Unmatched,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DegradedDetail {
    pub required: ProficiencyLevel,
    pub served: ProficiencyLevel,
}

// -----------------------------------------------------------------
// Section 6 — Knowledge Search API
// -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchRequest {
    pub query_embedding: Vec<f32>,
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
}

// -----------------------------------------------------------------
// Helper functions (non-normative)
//
// The spec leaves the scoring algorithm to the implementation (§3.2).
// These helpers are the reference behavior shared by Sovereign and
// Commonwealth so cross-project tests agree on numeric scores.
// -----------------------------------------------------------------

/// Returns the proficiency for a capability, defaulting to 0 if absent
/// or if the capability deserialized to `Unknown` (§2.4).
pub fn proficiency(profile: &CapabilityProfile, cap: Capability) -> ProficiencyLevel {
    if matches!(cap, Capability::Unknown) {
        return 0;
    }
    profile.get(&cap).copied().unwrap_or(0)
}

/// Returns true if `model_caps` meets every required threshold (§3.2).
pub fn satisfies_required(
    model_caps: &CapabilityProfile,
    required: &CapabilityProfile,
) -> bool {
    required.iter().all(|(cap, &min_level)| {
        if matches!(cap, Capability::Unknown) {
            // Unknown requirements are ignored per §2.4 ignorance-safety.
            return true;
        }
        proficiency(model_caps, *cap) >= min_level
    })
}

/// Score `model_caps` against `preferred`. Higher is better.
/// Returns the average per-capability ratio (capped at 1.0 each), or 0.0
/// if `preferred` is empty. Unknown capabilities are skipped.
pub fn score_preferred(
    model_caps: &CapabilityProfile,
    preferred: &CapabilityProfile,
) -> f32 {
    let counted: Vec<(Capability, ProficiencyLevel)> = preferred
        .iter()
        .filter(|(cap, _)| !matches!(cap, Capability::Unknown))
        .map(|(cap, &want)| (*cap, want))
        .collect();

    if counted.is_empty() {
        return 0.0;
    }

    let total: f32 = counted
        .iter()
        .map(|(cap, want)| {
            if *want == 0 {
                return 0.0;
            }
            let have = proficiency(model_caps, *cap) as f32;
            (have / *want as f32).min(1.0)
        })
        .sum();
    total / counted.len() as f32
}

// -----------------------------------------------------------------
// v0.3 — Translation helpers (legacy v0.2 ↔ claim-based)
// -----------------------------------------------------------------

/// Derive a [`CapabilityHint`] from a legacy v0.2 [`CapabilityProfile`].
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
///
/// Used by the scheduler to synthesize a v0.3 hint for peers that
/// only advertise v0.2 `capabilities` (graceful degradation path
/// per v0.3 §10.3).
pub fn infer_hint_from_profile(
    profile: &CapabilityProfile,
) -> CapabilityHint {
    let code = proficiency(profile, Capability::Code);
    let general = proficiency(profile, Capability::General);
    if code == 4 && code > general {
        CapabilityHint::code()
    } else {
        CapabilityHint::general()
    }
}

// -----------------------------------------------------------------
// v0.3 §6 — Reference scoring function
// -----------------------------------------------------------------

/// Hint-match score when a request hint doesn't equal the claim hint
/// but the claim offers `general` (every claim implicitly satisfies
/// `general` per v0.3 §4.1 — "every node serving inference must
/// support this hint as a minimum"). Chosen to be decisively worse
/// than an exact match (1.0) yet noticeably better than a wrong
/// specialization (0.0) so the scheduler prefers any node with the
/// requested specialty over a general fallback, but still routes work
/// somewhere if no specialist is reachable.
pub const HINT_GENERAL_FALLBACK_SCORE: f32 = 0.5;

/// Latency-match score when claim and request classes are one class
/// apart (fast↔normal or normal↔extended). Latency mismatch is a soft
/// deprioritization per v0.3 §5 — a node advertising fast work can
/// still serve normal work, just with a weaker fit.
pub const LATENCY_ADJACENT_SCORE: f32 = 0.8;

/// Latency-match score when claim and request classes are two apart
/// (fast↔extended). The widest soft deprioritization.
pub const LATENCY_TWO_CLASS_SCORE: f32 = 0.5;

/// Score how well a claim's `hint` covers a request for `req_hint`.
///
/// - Exact match (same standardized hint, or same extension hint) →
///   `1.0`.
/// - Request specific (e.g., `code`, `x:prose`), claim `general` →
///   [`HINT_GENERAL_FALLBACK_SCORE`] (0.5) — the documented spec §4.2
///   fallback: "falling back to general when no node advertises the
///   requested hint."
/// - Every other non-match → `0.0`. In particular, a request for
///   `general` against a specific-hint claim (code, x:prose, …) is
///   **not** a free 1.0. The spec §4.1 requirement "every node
///   serving inference must support general as a minimum" is an
///   obligation on the **advertiser**: a node that wants to serve
///   general work must publish a general claim. Scoring a code-
///   specialist claim at 1.0 for a general request would subvert
///   that obligation and let a specialist silently absorb every
///   general-hinted request on the mesh.
///
/// Advertisers that want their node to serve both their specialty
/// and general work should publish two claims (one specific, one
/// general) — see `sovereign-mesh::inference_adapter::synthesize_slot_claims`
/// for the reference pattern.
pub fn hint_match_score(
    claim_hint: &CapabilityHint,
    req_hint: &CapabilityHint,
) -> f32 {
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
    // All other mismatches (request general vs specific claim;
    // two different specifics) are zero score → eliminated from
    // ranking by the scheduler.
    0.0
}

/// Score how well a claim's `latency_class` covers a request for
/// `req_class`.
///
/// - Exact match → `1.0`.
/// - Adjacent class → [`LATENCY_ADJACENT_SCORE`] (0.8).
/// - Two-class gap → [`LATENCY_TWO_CLASS_SCORE`] (0.5).
pub fn latency_match_score(
    claim_class: LatencyClass,
    req_class: LatencyClass,
) -> f32 {
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

/// Score a candidate claim against a request (v0.3 §6).
///
/// Applies the protocol-level portion of the full scoring function:
///
/// ```text
/// hint_match × context_fits × output_fits × latency_match × affinity
/// ```
///
/// Returns `None` when the claim fails a hard feasibility gate
/// (context or output capacity exceeded) — such a claim is eliminated
/// from ranking, not merely deprioritized. Returns `Some(score)` in
/// `[0.0, 1.0]` otherwise.
///
/// Schedulers apply their own locality bonus, load penalty, and
/// observation-adjusted affinity *outside* this function. Keeping
/// those concerns local means every scheduler agrees on the
/// protocol-derived portion of the score, while each remains free to
/// encode its own operational view (LAN vs WAN, per-node load
/// history, cold-start ramps) without the protocol dictating their
/// shape.
pub fn score_claim_for_request(
    claim: &CapabilityClaim,
    req: &InferenceRequirements,
) -> Option<f32> {
    // Hard gates first per §6: a claim that cannot fit the request
    // is eliminated before any soft scoring runs. Absent request
    // fields (None) mean "unknown" — treat as passing the gate so
    // v0.2 clients that don't populate context_tokens /
    // max_output_tokens still get routed.
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
        // Wrong specialization — formally produces a zero score.
        // Return None so the scheduler elides the candidate instead
        // of ranking a useless zero.
        return None;
    }

    let latency = latency_match_score(
        claim.latency_class,
        req.effective_latency_class(),
    );

    Some(hint * latency * claim.effective_affinity())
}

/// Translate a legacy v0.2 [`LatencyPreference`] into a v0.3
/// [`LatencyClass`].
///
/// - `Interactive` → `Fast` (UI-speed requirement).
/// - `Throughput` → `Extended` (user explicitly accepts higher TTFT
///   in exchange for bulk throughput — the system can pick a slower,
///   higher-quality path).
/// - `Background` → `Extended` (offline / deferred work).
/// - `BestEffort` → `Normal` (the spec default — "whatever is
///   reasonable").
///
/// Used when an old client sends only `performance.latency` and the
/// scheduler needs a v0.3 latency class to score candidate claims.
pub fn latency_class_from_preference(
    pref: LatencyPreference,
) -> LatencyClass {
    match pref {
        LatencyPreference::Interactive => LatencyClass::Fast,
        LatencyPreference::Throughput
        | LatencyPreference::Background => LatencyClass::Extended,
        LatencyPreference::BestEffort => LatencyClass::Normal,
    }
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(entries: &[(Capability, u8)]) -> CapabilityProfile {
        entries.iter().copied().collect()
    }

    #[test]
    fn version_constant_matches_spec() {
        // v0.3 added claim-based routing additively; v0.2 consumers
        // continue to interop with reduced routing precision.
        assert_eq!(OICP_VERSION, "0.3.0");
    }

    #[test]
    fn satisfies_required_basic() {
        let model = caps(&[(Capability::Code, 4), (Capability::General, 2)]);
        let req = caps(&[(Capability::Code, 2)]);
        assert!(satisfies_required(&model, &req));

        let req = caps(&[(Capability::Code, 4)]);
        assert!(satisfies_required(&model, &req));

        let req = caps(&[(Capability::Analysis, 1)]);
        assert!(!satisfies_required(&model, &req));
    }

    #[test]
    fn satisfies_required_empty_required_always_true() {
        let model = CapabilityProfile::new();
        let req = CapabilityProfile::new();
        assert!(satisfies_required(&model, &req));
    }

    #[test]
    fn score_preferred_average_of_ratios() {
        let model = caps(&[(Capability::Code, 4), (Capability::Instruction, 3)]);
        let pref = caps(&[(Capability::Code, 4), (Capability::Instruction, 4)]);
        // 4/4 = 1.0; 3/4 = 0.75; mean = 0.875
        let score = score_preferred(&model, &pref);
        assert!((score - 0.875).abs() < 1e-4, "got {score}");
    }

    #[test]
    fn score_preferred_caps_at_one() {
        let model = caps(&[(Capability::Code, 4)]);
        let pref = caps(&[(Capability::Code, 2)]);
        let score = score_preferred(&model, &pref);
        assert!((score - 1.0).abs() < 1e-4);
    }

    #[test]
    fn score_preferred_empty_preferred_is_zero() {
        let model = CapabilityProfile::new();
        let pref = CapabilityProfile::new();
        assert_eq!(score_preferred(&model, &pref), 0.0);
    }

    #[test]
    fn unknown_capability_deserializes_and_is_ignored_in_scoring() {
        let json = r#"{"future_capability": 3, "code": 4}"#;
        let profile: CapabilityProfile = serde_json::from_str(json).unwrap();
        // The unknown key collapsed to Unknown in the map.
        assert_eq!(proficiency(&profile, Capability::Code), 4);
        assert_eq!(proficiency(&profile, Capability::Unknown), 0);

        // satisfies_required ignores Unknown thresholds.
        let req: CapabilityProfile =
            serde_json::from_str(r#"{"future_capability": 4}"#).unwrap();
        let model = caps(&[]);
        assert!(satisfies_required(&model, &req));
    }

    #[test]
    fn requirements_default_local_only() {
        let req = InferenceRequirements::default();
        assert_eq!(req.oicp_version, OICP_VERSION);
        assert_eq!(req.sharding(), ShardingPrivacy::LocalOnly);
        assert_eq!(req.latency(), LatencyPreference::BestEffort);
        assert!(req.required().is_empty());
        assert!(req.preferred().is_empty());
    }

    #[test]
    fn requirements_builders_compose() {
        let req = InferenceRequirements::new()
            .with_capabilities(CapabilityRequirements {
                required: caps(&[(Capability::Code, 2)]),
                preferred: caps(&[(Capability::Code, 4), (Capability::Instruction, 3)]),
            })
            .with_context(ContextRequirements {
                min_tokens: Some(8192),
                preferred_tokens: Some(32768),
            })
            .with_latency(LatencyPreference::Interactive)
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_request_id("test-req");

        assert_eq!(req.latency(), LatencyPreference::Interactive);
        assert_eq!(req.sharding(), ShardingPrivacy::MeshAllowed);
        assert_eq!(req.min_tokens(), Some(8192));
        assert_eq!(req.preferred_tokens(), Some(32768));
        assert_eq!(req.required().get(&Capability::Code), Some(&2));
        assert_eq!(req.preferred().get(&Capability::Instruction), Some(&3));
        assert_eq!(req.request_id.as_deref(), Some("test-req"));
    }

    #[test]
    fn requirements_serialize_in_spec_shape() {
        let req = InferenceRequirements::new()
            .with_capabilities(CapabilityRequirements {
                required: caps(&[(Capability::Code, 2)]),
                preferred: caps(&[(Capability::Code, 3)]),
            })
            .with_latency(LatencyPreference::Interactive)
            .with_sharding(ShardingPrivacy::MeshAllowed);

        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["oicp_version"], OICP_VERSION);
        assert_eq!(value["capabilities"]["required"]["code"], 2);
        assert_eq!(value["capabilities"]["preferred"]["code"], 3);
        assert_eq!(value["performance"]["latency"], "interactive");
        assert_eq!(value["privacy"]["sharding"], "mesh_allowed");
    }

    #[test]
    fn requirements_round_trip_minimal_request() {
        // The spec says the only required field is oicp_version. A minimal
        // request with just that should round-trip cleanly.
        let req = InferenceRequirements::new();
        let json = serde_json::to_string(&req).unwrap();
        let back: InferenceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(back.oicp_version, OICP_VERSION);
        assert!(back.capabilities.is_none());
        assert!(back.context.is_none());
        assert!(back.performance.is_none());
        assert!(back.privacy.is_none());
    }

    #[test]
    fn manifest_round_trip_with_knowledge_and_federation() {
        let json = r#"{
            "oicp_version": "0.2.0",
            "provider": {"name": "Test Co-op", "type": "mesh"},
            "models": [
                {
                    "id": "qwen3-coder-30b-q4km",
                    "base_model": "qwen3-coder-30b",
                    "quantization": "Q4_K_M",
                    "capabilities": {"general": 2, "code": 4, "instruction": 3},
                    "context_tokens": 32768,
                    "status": {
                        "available": true,
                        "loaded": true,
                        "estimated_tokens_per_sec": 45.0,
                        "estimated_ttft_ms": 1100
                    }
                }
            ],
            "knowledge": {
                "corpora": [
                    {
                        "id": "wikipedia",
                        "total_chunks": 6800000,
                        "shards": 3,
                        "replicas": 2,
                        "fully_available": true
                    }
                ],
                "search_endpoint": "/v1/knowledge/search"
            },
            "federation": {
                "peers": [
                    {
                        "name": "Mission District Co-op",
                        "capabilities_url": "http://10.0.1.50:9741/oicp/v1/capabilities",
                        "trust_level": "model_and_knowledge_sharing"
                    }
                ]
            }
        }"#;

        let manifest: ProviderManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.oicp_version, "0.2.0");
        assert_eq!(manifest.models.len(), 1);
        assert_eq!(manifest.models[0].base_model.as_deref(), Some("qwen3-coder-30b"));
        assert_eq!(manifest.models[0].quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(
            proficiency(&manifest.models[0].capabilities, Capability::Code),
            4
        );
        assert!(manifest.models[0].status.loaded);

        let knowledge = manifest.knowledge.expect("knowledge present");
        assert_eq!(knowledge.corpora.len(), 1);
        assert_eq!(knowledge.corpora[0].id, "wikipedia");
        assert_eq!(knowledge.corpora[0].total_chunks, 6_800_000);
        assert_eq!(knowledge.search_endpoint, "/v1/knowledge/search");

        let federation = manifest.federation.expect("federation present");
        assert_eq!(federation.peers.len(), 1);
        assert_eq!(federation.peers[0].name, "Mission District Co-op");
    }

    #[test]
    fn response_meta_round_trip_with_degradation() {
        let mut degraded = HashMap::new();
        degraded.insert(
            Capability::Analysis,
            DegradedDetail {
                required: 3,
                served: 2,
            },
        );
        let meta = OicpResponseMeta {
            model_capabilities: Some(caps(&[(Capability::Code, 4)])),
            quantization: Some("Q4_K_M".into()),
            match_quality: Some(MatchQuality::Degraded),
            degraded_capabilities: Some(degraded),
            request_id: Some("step-4-synthesis".into()),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: OicpResponseMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.match_quality, Some(MatchQuality::Degraded));
        assert_eq!(back.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(
            back.degraded_capabilities.unwrap()[&Capability::Analysis].served,
            2
        );
    }

    #[test]
    fn knowledge_search_request_default_limit() {
        let req: KnowledgeSearchRequest = serde_json::from_str(
            r#"{"query_embedding": [0.1, 0.2], "query_text": "Ostrom"}"#,
        )
        .unwrap();
        assert_eq!(req.effective_limit(), KnowledgeSearchRequest::DEFAULT_LIMIT);
        assert!(req.corpora.is_none());
    }

    #[test]
    fn knowledge_search_response_round_trip() {
        let resp = KnowledgeSearchResponse {
            results: vec![KnowledgeResult {
                content: "Elinor Ostrom identified eight design principles...".into(),
                title: Some("Elinor Ostrom".into()),
                corpus_id: "wikipedia".into(),
                url: Some("https://en.wikipedia.org/wiki/Elinor_Ostrom".into()),
                score: 0.89,
                metadata: HashMap::from([("section".into(), "Design principles".into())]),
            }],
            corpora_searched: vec!["wikipedia".into()],
            corpora_unavailable: vec![],
            total_chunks_searched: Some(6_800_000),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: KnowledgeSearchResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.results.len(), 1);
        assert_eq!(back.results[0].title.as_deref(), Some("Elinor Ostrom"));
        assert_eq!(back.corpora_searched, vec!["wikipedia"]);
    }

    // -----------------------------------------------------------
    // v0.3 routing — CapabilityHint / LatencyClass / CapabilityClaim
    // -----------------------------------------------------------

    #[test]
    fn capability_hint_standardized_constructors() {
        let g = CapabilityHint::general();
        assert_eq!(g.as_str(), "general");
        assert!(g.is_standardized());
        assert!(!g.is_extension());
        assert!(!g.is_unknown_bare());

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
            CapabilityHint::extension("   ").unwrap_err(),
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
        // Standardized hints round-trip verbatim.
        assert_eq!(CapabilityHint::parse("general").unwrap().as_str(), "general");
        assert_eq!(CapabilityHint::parse("code").unwrap().as_str(), "code");

        // Extension hints preserve their prefix.
        let ext = CapabilityHint::parse("x:biomed").unwrap();
        assert_eq!(ext.as_str(), "x:biomed");
        assert!(ext.is_extension());

        // A bare string we don't recognize is preserved as a
        // future-standardized candidate (graceful degradation, §10.3).
        let future = CapabilityHint::parse("math").unwrap();
        assert_eq!(future.as_str(), "math");
        assert!(future.is_unknown_bare());
        assert!(!future.is_standardized());
        assert!(!future.is_extension());

        // Empty / whitespace are rejected.
        assert!(CapabilityHint::parse("").is_err());
        assert!(CapabilityHint::parse("   ").is_err());
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
            // Plain quoted string, no object wrapping.
            assert!(json.starts_with('"') && json.ends_with('"'), "got {json}");
            let back: CapabilityHint = serde_json::from_str(&json).unwrap();
            assert_eq!(back, h);
        }
    }

    #[test]
    fn capability_hint_serde_rejects_structurally_bad_input() {
        let err = serde_json::from_str::<CapabilityHint>(r#""""#);
        assert!(err.is_err());
        let err = serde_json::from_str::<CapabilityHint>(r#""has space""#);
        assert!(err.is_err());
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
    fn capability_claim_fits_is_gate_on_context_and_output() {
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
        assert!((v["affinity"].as_f64().unwrap() - 0.9).abs() < 1e-5);

        let back: CapabilityClaim = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hint, claim.hint);
        assert_eq!(back.latency_class, claim.latency_class);
        assert_eq!(back.max_context, claim.max_context);
        assert_eq!(back.max_output, claim.max_output);
    }

    // -----------------------------------------------------------
    // v0.3 routing — InferenceRequirements property fields
    // -----------------------------------------------------------

    #[test]
    fn requirements_v03_builders_and_defaults() {
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Fast)
            .with_context_tokens(16_000)
            .with_max_output_tokens(2_000);
        assert_eq!(req.capability_hint, Some(CapabilityHint::code()));
        assert_eq!(req.latency_class, Some(LatencyClass::Fast));
        assert_eq!(req.context_tokens, Some(16_000));
        assert_eq!(req.max_output_tokens, Some(2_000));
        assert_eq!(req.effective_hint(), CapabilityHint::code());
        assert_eq!(req.effective_latency_class(), LatencyClass::Fast);

        // Absent fields default to general / Normal per v0.3 §8.
        let bare = InferenceRequirements::new();
        assert_eq!(bare.effective_hint(), CapabilityHint::general());
        assert_eq!(bare.effective_latency_class(), LatencyClass::Normal);
    }

    #[test]
    fn requirements_v02_payload_deserializes_into_v03_types() {
        // A v0.2 client sends a payload without any of the new v0.3
        // routing fields. It must deserialize cleanly into the
        // extended struct, with the new fields absent (None), and
        // effective_* accessors fall back to the spec defaults.
        let v02_json = r#"{
            "oicp_version": "0.2.0",
            "capabilities": {
                "required": {"code": 2},
                "preferred": {"code": 4, "instruction": 3}
            },
            "performance": {"latency": "interactive"},
            "privacy": {"sharding": "mesh_allowed"}
        }"#;
        let req: InferenceRequirements = serde_json::from_str(v02_json).unwrap();
        assert!(req.capability_hint.is_none());
        assert!(req.latency_class.is_none());
        assert!(req.context_tokens.is_none());
        assert!(req.max_output_tokens.is_none());
        assert_eq!(req.effective_hint(), CapabilityHint::general());
        assert_eq!(req.effective_latency_class(), LatencyClass::Normal);
        // v0.2 fields survive.
        assert_eq!(req.required().get(&Capability::Code), Some(&2));
        assert_eq!(req.sharding(), ShardingPrivacy::MeshAllowed);
    }

    #[test]
    fn requirements_mixed_v02_and_v03_fields_coexist() {
        // A v0.3-aware client carrying both the legacy v0.2
        // capabilities profile and the new v0.3 hint must serialize
        // and round-trip without collision.
        let req = InferenceRequirements::new()
            .with_capabilities(CapabilityRequirements {
                required: caps(&[(Capability::Code, 2)]),
                preferred: caps(&[(Capability::Code, 4)]),
            })
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Normal)
            .with_context_tokens(8_000)
            .with_max_output_tokens(1_500);
        let json = serde_json::to_string(&req).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["capabilities"]["required"]["code"], 2);
        assert_eq!(v["capability_hint"], "code");
        assert_eq!(v["latency_class"], "normal");
        assert_eq!(v["context_tokens"], 8_000);
        assert_eq!(v["max_output_tokens"], 1_500);

        let back: InferenceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(back.capability_hint, Some(CapabilityHint::code()));
        assert_eq!(back.required().get(&Capability::Code), Some(&2));
    }

    // -----------------------------------------------------------
    // v0.3 routing — ProviderModel.claims coexistence with v0.2
    // -----------------------------------------------------------

    #[test]
    fn provider_model_v02_deserializes_with_empty_claims() {
        // A v0.2 manifest has no `claims` key. It must still
        // deserialize into the v0.3 type, with `claims` defaulting
        // to an empty vec (consumers then fall back to the legacy
        // `capabilities` profile).
        let v02_json = r#"{
            "id": "qwen3-9b",
            "capabilities": {"general": 3, "code": 2},
            "context_tokens": 16384,
            "status": {"available": true, "loaded": true}
        }"#;
        let model: ProviderModel = serde_json::from_str(v02_json).unwrap();
        assert_eq!(model.id, "qwen3-9b");
        assert!(model.claims.is_empty());
        assert_eq!(
            proficiency(&model.capabilities, Capability::General),
            3
        );
    }

    #[test]
    fn provider_model_v03_manifest_round_trips_with_claims() {
        let model = ProviderModel {
            id: "qwen3-9b".into(),
            base_model: None,
            quantization: Some("Q4_K_M".into()),
            capabilities: caps(&[
                (Capability::General, 3),
                (Capability::Code, 2),
            ]),
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
        };

        let json = serde_json::to_string(&model).unwrap();
        assert!(json.contains(r#""claims":"#));
        assert!(json.contains(r#""hint":"general""#));

        let back: ProviderModel = serde_json::from_str(&json).unwrap();
        assert_eq!(back.claims.len(), 2);
        assert_eq!(back.claims[0].latency_class, LatencyClass::Fast);
        assert_eq!(back.claims[1].latency_class, LatencyClass::Normal);
    }

    #[test]
    fn provider_model_empty_claims_omitted_from_json() {
        // skip_serializing_if = "Vec::is_empty" — v0.2 consumers
        // reading a v0.3 manifest shouldn't see an unexpected key.
        let model = ProviderModel {
            id: "qwen3-9b".into(),
            base_model: None,
            quantization: None,
            capabilities: caps(&[(Capability::General, 3)]),
            context_tokens: 16_384,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: None,
            claims: Vec::new(),
        };
        let json = serde_json::to_string(&model).unwrap();
        assert!(!json.contains("claims"), "empty claims must be omitted: {json}");
    }

    // -----------------------------------------------------------
    // v0.3 routing — translation helpers
    // -----------------------------------------------------------

    #[test]
    fn infer_hint_requires_code_exceptional_and_exceeding_general() {
        // Well-rounded model with equal Strong across all → general.
        assert_eq!(
            infer_hint_from_profile(&caps(&[
                (Capability::Code, 3),
                (Capability::General, 3),
                (Capability::Analysis, 3),
            ])),
            CapabilityHint::general()
        );
        // Only Exceptional code with general below it → code.
        assert_eq!(
            infer_hint_from_profile(&caps(&[
                (Capability::Code, 4),
                (Capability::General, 2),
            ])),
            CapabilityHint::code()
        );
        // Exceptional code AND equally-exceptional general →
        // still general. Tie doesn't establish specialization.
        assert_eq!(
            infer_hint_from_profile(&caps(&[
                (Capability::Code, 4),
                (Capability::General, 4),
            ])),
            CapabilityHint::general()
        );
        // Code at Strong with no general → not a specialist; a
        // balanced model with `code = 3` alone is interpreted as a
        // general model with solid code chops, per v0.3 §4.4.
        assert_eq!(
            infer_hint_from_profile(&caps(&[(Capability::Code, 3)])),
            CapabilityHint::general()
        );
        // Empty profile → general.
        assert_eq!(
            infer_hint_from_profile(&CapabilityProfile::new()),
            CapabilityHint::general()
        );
    }

    #[test]
    fn latency_class_from_preference_maps_all_variants() {
        assert_eq!(
            latency_class_from_preference(LatencyPreference::Interactive),
            LatencyClass::Fast
        );
        assert_eq!(
            latency_class_from_preference(LatencyPreference::Throughput),
            LatencyClass::Extended
        );
        assert_eq!(
            latency_class_from_preference(LatencyPreference::Background),
            LatencyClass::Extended
        );
        assert_eq!(
            latency_class_from_preference(LatencyPreference::BestEffort),
            LatencyClass::Normal
        );
    }

    // -----------------------------------------------------------
    // v0.3 §6 — Scoring function
    // -----------------------------------------------------------

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
            hint_match_score(
                &CapabilityHint::general(),
                &CapabilityHint::general()
            ),
            1.0
        );
        assert_eq!(
            hint_match_score(
                &CapabilityHint::extension("prose").unwrap(),
                &CapabilityHint::extension("prose").unwrap()
            ),
            1.0
        );
    }

    #[test]
    fn hint_match_general_request_against_specific_claim_is_zero() {
        // Specialization obligation is on the advertiser: a node
        // that wants to serve general work must publish a general
        // claim. A specific-hint claim (code, x:biomed, …) against
        // a general request scores 0.0 so the scheduler doesn't
        // silently route general work to a specialist.
        assert_eq!(
            hint_match_score(
                &CapabilityHint::code(),
                &CapabilityHint::general()
            ),
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
        // Documented fallback path (§4.2): request code, only general
        // available — take the hit.
        assert_eq!(
            hint_match_score(
                &CapabilityHint::general(),
                &CapabilityHint::code()
            ),
            HINT_GENERAL_FALLBACK_SCORE
        );
        assert_eq!(
            hint_match_score(
                &CapabilityHint::general(),
                &CapabilityHint::extension("prose").unwrap()
            ),
            HINT_GENERAL_FALLBACK_SCORE
        );
    }

    #[test]
    fn hint_match_specific_vs_different_specific_is_zero() {
        // Wrong specialization — worse than no specialization.
        assert_eq!(
            hint_match_score(
                &CapabilityHint::code(),
                &CapabilityHint::extension("prose").unwrap()
            ),
            0.0
        );
        assert_eq!(
            hint_match_score(
                &CapabilityHint::extension("prose").unwrap(),
                &CapabilityHint::code()
            ),
            0.0
        );
        assert_eq!(
            hint_match_score(
                &CapabilityHint::extension("biomed").unwrap(),
                &CapabilityHint::extension("prose").unwrap()
            ),
            0.0
        );
    }

    #[test]
    fn latency_match_exact_adjacent_and_two_class_gap() {
        // Exact.
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Fast),
            1.0
        );
        // Adjacent.
        assert_eq!(
            latency_match_score(LatencyClass::Fast, LatencyClass::Normal),
            LATENCY_ADJACENT_SCORE
        );
        assert_eq!(
            latency_match_score(
                LatencyClass::Normal,
                LatencyClass::Extended
            ),
            LATENCY_ADJACENT_SCORE
        );
        // Two-class gap is symmetric.
        assert_eq!(
            latency_match_score(
                LatencyClass::Fast,
                LatencyClass::Extended
            ),
            LATENCY_TWO_CLASS_SCORE
        );
        assert_eq!(
            latency_match_score(
                LatencyClass::Extended,
                LatencyClass::Fast
            ),
            LATENCY_TWO_CLASS_SCORE
        );
    }

    #[test]
    fn score_hard_gate_eliminates_insufficient_context() {
        let c = claim(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_000, // claim max_context
            2_000,
            0.9,
        );
        let over_ctx = req_with(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_001, // request needs more than claim offers
            1_000,
        );
        assert_eq!(score_claim_for_request(&c, &over_ctx), None);
    }

    #[test]
    fn score_hard_gate_eliminates_insufficient_output() {
        let c = claim(
            CapabilityHint::general(),
            LatencyClass::Normal,
            16_000,
            1_000, // claim max_output
            0.9,
        );
        let over_out = req_with(
            CapabilityHint::general(),
            LatencyClass::Normal,
            8_000,
            1_001, // request needs more output than claim serves
        );
        assert_eq!(score_claim_for_request(&c, &over_out), None);
    }

    #[test]
    fn score_hard_gate_passes_when_request_omits_size_fields() {
        // A v0.2-style request with no context_tokens /
        // max_output_tokens should not be rejected by the hard gate.
        let c = claim(
            CapabilityHint::general(),
            LatencyClass::Normal,
            4_000,
            500,
            0.8,
        );
        let req = InferenceRequirements::new(); // no sizes
        let score = score_claim_for_request(&c, &req).expect("passes gate");
        // Default request hint → general, default latency → Normal.
        // Both exact matches, affinity 0.8.
        assert!((score - 0.8).abs() < 1e-6);
    }

    #[test]
    fn score_wrong_specialization_returns_none() {
        // Request `code`, claim `x:prose` — zero hint score — None.
        let c = claim(
            CapabilityHint::extension("prose").unwrap(),
            LatencyClass::Normal,
            16_000,
            2_000,
            0.9,
        );
        let req = req_with(
            CapabilityHint::code(),
            LatencyClass::Normal,
            4_000,
            1_000,
        );
        assert_eq!(score_claim_for_request(&c, &req), None);
    }

    #[test]
    fn score_full_formula_multiplies_hint_latency_affinity() {
        // Request code/fast/small. Claim offers code/normal (adjacent
        // latency) with 0.9 affinity. Expected:
        //   hint (code=code = 1.0) × latency (fast→normal = 0.8) ×
        //   affinity (0.9) = 0.72
        let c = claim(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.9,
        );
        let req = req_with(
            CapabilityHint::code(),
            LatencyClass::Fast,
            4_000,
            500,
        );
        let score = score_claim_for_request(&c, &req).expect("passes");
        assert!((score - 0.72).abs() < 1e-6, "got {score}");
    }

    #[test]
    fn score_coder_collective_scenario_ranks_specialist_above_generalist() {
        // The spec's §6.2 coder-collective scenario: a code request
        // should score higher against the Qwen Coder claim than
        // against a 70B general claim, even though the general model
        // has high general affinity.
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
        let req = req_with(
            CapabilityHint::code(),
            LatencyClass::Normal,
            16_000,
            2_000,
        );
        let coder_score =
            score_claim_for_request(&qwen_coder, &req).unwrap();
        let llama_score =
            score_claim_for_request(&llama_70b, &req).unwrap();
        assert!(
            coder_score > llama_score,
            "code specialist ({coder_score}) must beat general fallback \
             ({llama_score}) for code-hinted requests"
        );
        // Sanity: exact hint & latency match on coder → raw affinity.
        assert!((coder_score - 0.95).abs() < 1e-6);
        // Sanity: general fallback gives 0.5 × 1.0 × 0.85 = 0.425.
        assert!((llama_score - 0.425).abs() < 1e-6);
    }
}
