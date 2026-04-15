//! OICP — Open Inference Capabilities Protocol v0.2.0
//!
//! Canonical types per the specification at
//! `commonwealth/docs/oicp-v0.2.md`. This crate is the single source of
//! truth for all OICP type definitions, consumed by both the Sovereign
//! (`lcol-llm`) and Commonwealth workspaces via path dependency.
//!
//! Section references in this file refer to oicp-v0.2.md.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// OICP specification version implemented by this module.
pub const OICP_VERSION: &str = "0.2.0";

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
// Section 3 — Client Requirements Schema
// -----------------------------------------------------------------

/// What a client needs from an inference call (§3).
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeManifest {
    pub corpora: Vec<CorpusDescriptor>,
    pub search_endpoint: String,
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
        assert_eq!(OICP_VERSION, "0.2.0");
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
        assert_eq!(value["oicp_version"], "0.2.0");
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
}
