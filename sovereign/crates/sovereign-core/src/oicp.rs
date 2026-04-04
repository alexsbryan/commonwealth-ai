use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// OICP capability domains. Extensible — unknown variants deserialize
/// to `Unknown` and are ignored in scoring.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Proficiency level. 0-4 integer scale.
/// None(0), Basic(1), Moderate(2), Strong(3), Exceptional(4).
pub type ProficiencyLevel = u8;

/// A model's capability profile: map from capability to proficiency.
pub type CapabilityProfile = HashMap<Capability, ProficiencyLevel>;

/// What the client needs from the inference provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceRequirements {
    /// Minimum proficiency levels. Provider MUST NOT select a model
    /// below any of these thresholds.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub required: CapabilityProfile,

    /// Desired proficiency levels. Used for scoring among models
    /// that meet the required thresholds.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub preferred: CapabilityProfile,

    /// Minimum context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_context_tokens: Option<usize>,

    /// Latency preference.
    #[serde(default)]
    pub latency: LatencyPreference,

    /// Privacy constraint.
    #[serde(default)]
    pub privacy: PrivacyPreference,

    /// Grounding preference for knowledge injection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding: Option<GroundingPreference>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyPreference {
    Interactive,
    Throughput,
    Background,
    #[default]
    BestEffort,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyPreference {
    /// Never shard across nodes. Run on a single node.
    #[default]
    LocalOnly,
    /// Allow distributed sharding for better model quality.
    MeshAllowed,
}

/// Grounding preference for knowledge injection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingPreference {
    /// Client handles its own knowledge grounding (Sovereign).
    ClientManaged,
    /// Provider should inject knowledge context (Commonwealth).
    ProviderManaged,
}

/// Metadata returned by an OICP-aware provider in the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OicpResponseMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_capabilities: Option<CapabilityProfile>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_quality: Option<MatchQuality>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_capabilities: Option<HashMap<Capability, DegradedDetail>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchQuality {
    Full,
    Partial,
    Degraded,
    Unmatched,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DegradedDetail {
    pub required: ProficiencyLevel,
    pub served: ProficiencyLevel,
}

/// A model entry from the provider's capability manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    pub capabilities: CapabilityProfile,
    pub context_tokens: usize,
    pub status: ModelStatus,
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

/// The full provider manifest returned by GET /oicp/v1/capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderManifest {
    pub oicp_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderInfo>,
    pub models: Vec<ProviderModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

// ─── Scoring Functions ────────────────────────────────────────

/// Check if a model meets all required capability thresholds.
pub fn satisfies_required(model: &ProviderModel, required: &CapabilityProfile) -> bool {
    required.iter().all(|(cap, &min_level)| {
        model.capabilities.get(cap).copied().unwrap_or(0) >= min_level
    })
}

/// Score a model against preferred capabilities.
/// Returns a value between 0.0 and 1.0 indicating how well the model
/// matches the preferred profile.
pub fn score_preferred(
    model_caps: &CapabilityProfile,
    preferred: &CapabilityProfile,
) -> f32 {
    if preferred.is_empty() {
        return 0.0;
    }
    let total: f32 = preferred
        .iter()
        .map(|(cap, &desired)| {
            let actual = model_caps.get(cap).copied().unwrap_or(0) as f32;
            let desired = desired as f32;
            (actual / desired.max(1.0)).min(1.0)
        })
        .sum();
    total / preferred.len() as f32
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_roundtrip() {
        let mut required = CapabilityProfile::new();
        required.insert(Capability::Code, 2);

        let mut preferred = CapabilityProfile::new();
        preferred.insert(Capability::Code, 3);
        preferred.insert(Capability::Instruction, 3);

        let req = InferenceRequirements {
            required,
            preferred,
            min_context_tokens: Some(8192),
            latency: LatencyPreference::Interactive,
            privacy: PrivacyPreference::MeshAllowed,
            grounding: Some(GroundingPreference::ClientManaged),
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: InferenceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.required.get(&Capability::Code), Some(&2));
        assert_eq!(parsed.preferred.get(&Capability::Code), Some(&3));
        assert_eq!(parsed.min_context_tokens, Some(8192));
        assert_eq!(parsed.latency, LatencyPreference::Interactive);
        assert_eq!(parsed.privacy, PrivacyPreference::MeshAllowed);
    }

    #[test]
    fn manifest_roundtrip() {
        let json = r#"{
            "oicp_version": "0.1.0",
            "provider": {"name": "Test Co-op", "type": "mesh"},
            "models": [
                {
                    "id": "qwen3-coder-30b-q4km",
                    "capabilities": {"general": 2, "code": 4, "instruction": 3},
                    "context_tokens": 32768,
                    "status": {
                        "available": true,
                        "loaded": true,
                        "estimated_tokens_per_sec": 45.0,
                        "estimated_ttft_ms": 1100
                    }
                }
            ]
        }"#;

        let manifest: ProviderManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.oicp_version, "0.1.0");
        assert_eq!(manifest.models.len(), 1);
        assert_eq!(manifest.models[0].id, "qwen3-coder-30b-q4km");
        assert_eq!(
            manifest.models[0].capabilities.get(&Capability::Code),
            Some(&4)
        );
        assert!(manifest.models[0].status.loaded);
    }

    #[test]
    fn response_meta_roundtrip() {
        let json = r#"{
            "model_capabilities": {"general": 2, "code": 4},
            "match_quality": "full"
        }"#;

        let meta: OicpResponseMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.match_quality, Some(MatchQuality::Full));
        assert_eq!(
            meta.model_capabilities.as_ref().unwrap().get(&Capability::Code),
            Some(&4)
        );
    }

    #[test]
    fn unknown_capability_ignored() {
        let json = r#"{"future_capability": 3, "code": 4}"#;
        let profile: CapabilityProfile = serde_json::from_str(json).unwrap();
        assert_eq!(profile.get(&Capability::Code), Some(&4));
        assert_eq!(profile.get(&Capability::Unknown), Some(&3));
    }

    #[test]
    fn default_requirements_empty() {
        let req = InferenceRequirements::default();
        assert!(req.required.is_empty());
        assert!(req.preferred.is_empty());
        assert_eq!(req.latency, LatencyPreference::BestEffort);
        assert_eq!(req.privacy, PrivacyPreference::LocalOnly);
    }

    #[test]
    fn satisfies_required_all_met() {
        let mut required = CapabilityProfile::new();
        required.insert(Capability::Code, 2);
        required.insert(Capability::General, 1);

        let mut caps = CapabilityProfile::new();
        caps.insert(Capability::Code, 4);
        caps.insert(Capability::General, 3);

        let model = ProviderModel {
            id: "test".to_string(),
            quantization: None,
            capabilities: caps,
            context_tokens: 32768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
        };

        assert!(satisfies_required(&model, &required));
    }

    #[test]
    fn satisfies_required_not_met() {
        let mut required = CapabilityProfile::new();
        required.insert(Capability::Code, 3);

        let mut caps = CapabilityProfile::new();
        caps.insert(Capability::Code, 2);

        let model = ProviderModel {
            id: "test".to_string(),
            quantization: None,
            capabilities: caps,
            context_tokens: 32768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
        };

        assert!(!satisfies_required(&model, &required));
    }

    #[test]
    fn score_preferred_perfect_match() {
        let mut caps = CapabilityProfile::new();
        caps.insert(Capability::Code, 4);
        caps.insert(Capability::Instruction, 3);

        let mut preferred = CapabilityProfile::new();
        preferred.insert(Capability::Code, 4);
        preferred.insert(Capability::Instruction, 3);

        let score = score_preferred(&caps, &preferred);
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn score_preferred_partial_match() {
        let mut caps = CapabilityProfile::new();
        caps.insert(Capability::Code, 2);

        let mut preferred = CapabilityProfile::new();
        preferred.insert(Capability::Code, 4);

        let score = score_preferred(&caps, &preferred);
        assert!((score - 0.5).abs() < 0.001);
    }

    #[test]
    fn score_preferred_empty() {
        let caps = CapabilityProfile::new();
        let preferred = CapabilityProfile::new();
        assert_eq!(score_preferred(&caps, &preferred), 0.0);
    }
}
