use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// OICP capability IDs — the nine capability dimensions.
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
}

/// Proficiency level on a 0-4 scale.
pub type ProficiencyLevel = u8;

/// A map from capability IDs to proficiency levels.
/// Capabilities not present are implicitly level 0 (None).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityProfile(pub HashMap<Capability, ProficiencyLevel>);

impl CapabilityProfile {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn set(&mut self, capability: Capability, level: ProficiencyLevel) -> &mut Self {
        self.0.insert(capability, level);
        self
    }

    pub fn get(&self, capability: Capability) -> ProficiencyLevel {
        self.0.get(&capability).copied().unwrap_or(0)
    }

    /// Returns true if this profile meets all required minimum levels.
    pub fn satisfies(&self, required: &CapabilityProfile) -> bool {
        required
            .0
            .iter()
            .all(|(cap, &min_level)| self.get(*cap) >= min_level)
    }

    /// Score this profile against preferred capabilities.
    /// Higher is better. Simple weighted sum.
    pub fn score_against(&self, preferred: &CapabilityProfile) -> f32 {
        if preferred.0.is_empty() {
            return 0.0;
        }
        let total: f32 = preferred
            .0
            .iter()
            .map(|(cap, &wanted)| {
                let have = self.get(*cap) as f32;
                let want = wanted as f32;
                // Score: how close are we to the preferred level (0.0 to 1.0 per cap)
                if want == 0.0 {
                    0.0
                } else {
                    (have / want).min(1.0)
                }
            })
            .sum();
        total / preferred.0.len() as f32
    }
}

/// Client-side inference requirements per the OICP spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequirements {
    pub oicp_version: String,
    #[serde(default)]
    pub capabilities: CapabilityRequirements,
    #[serde(default)]
    pub context: Option<ContextRequirements>,
    #[serde(default)]
    pub performance: Option<PerformanceRequirements>,
    #[serde(default)]
    pub privacy: Option<PrivacyRequirements>,
}

/// Required and preferred capability levels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    #[serde(default)]
    pub required: CapabilityProfile,
    #[serde(default)]
    pub preferred: CapabilityProfile,
}

/// Context window requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRequirements {
    pub min_tokens: Option<u32>,
    pub preferred_tokens: Option<u32>,
}

/// Performance constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceRequirements {
    #[serde(default)]
    pub latency: LatencyPreference,
}

/// Latency preference for inference requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyPreference {
    Interactive,
    Throughput,
    Background,
    #[default]
    BestEffort,
}

/// Privacy requirements for inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyRequirements {
    pub sharding: ShardingPrivacy,
}

/// Whether the request may be processed across mesh nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardingPrivacy {
    /// Must not leave the local node. Commonwealth returns 400 if it receives this.
    LocalOnly,
    /// May be processed across the mesh.
    MeshAllowed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_profile_satisfies() {
        let mut model = CapabilityProfile::new();
        model.set(Capability::Code, 4);
        model.set(Capability::General, 2);

        let mut required = CapabilityProfile::new();
        required.set(Capability::Code, 2);
        assert!(model.satisfies(&required));

        required.set(Capability::Code, 4);
        assert!(model.satisfies(&required));

        required.set(Capability::Analysis, 1);
        assert!(
            !model.satisfies(&required),
            "model has no analysis capability"
        );
    }

    #[test]
    fn capability_profile_satisfies_empty_required() {
        let model = CapabilityProfile::new();
        let required = CapabilityProfile::new();
        assert!(model.satisfies(&required));
    }

    #[test]
    fn capability_profile_score() {
        let mut model = CapabilityProfile::new();
        model.set(Capability::Code, 4);
        model.set(Capability::Instruction, 3);

        let mut preferred = CapabilityProfile::new();
        preferred.set(Capability::Code, 4);
        preferred.set(Capability::Instruction, 4);

        let score = model.score_against(&preferred);
        // Code: 4/4 = 1.0, Instruction: 3/4 = 0.75, average = 0.875
        assert!((score - 0.875).abs() < 0.001);
    }

    #[test]
    fn capability_profile_score_empty_preferred() {
        let model = CapabilityProfile::new();
        let preferred = CapabilityProfile::new();
        assert_eq!(model.score_against(&preferred), 0.0);
    }

    #[test]
    fn capability_profile_score_exceeding_preferred() {
        let mut model = CapabilityProfile::new();
        model.set(Capability::Code, 4);

        let mut preferred = CapabilityProfile::new();
        preferred.set(Capability::Code, 2);

        let score = model.score_against(&preferred);
        // Capped at 1.0 per capability
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn inference_requirements_serde_roundtrip() {
        let req = InferenceRequirements {
            oicp_version: "0.1.0".into(),
            capabilities: CapabilityRequirements {
                required: {
                    let mut p = CapabilityProfile::new();
                    p.set(Capability::Code, 2);
                    p
                },
                preferred: {
                    let mut p = CapabilityProfile::new();
                    p.set(Capability::Code, 3);
                    p.set(Capability::Instruction, 3);
                    p
                },
            },
            context: Some(ContextRequirements {
                min_tokens: Some(8192),
                preferred_tokens: Some(32768),
            }),
            performance: Some(PerformanceRequirements {
                latency: LatencyPreference::Interactive,
            }),
            privacy: Some(PrivacyRequirements {
                sharding: ShardingPrivacy::MeshAllowed,
            }),
        };
        let json = serde_json::to_string_pretty(&req).unwrap();
        let back: InferenceRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(back.oicp_version, "0.1.0");
        assert_eq!(back.capabilities.required.get(Capability::Code), 2);
        assert_eq!(
            back.performance.unwrap().latency,
            LatencyPreference::Interactive
        );
    }

    #[test]
    fn latency_preference_default_is_best_effort() {
        assert_eq!(LatencyPreference::default(), LatencyPreference::BestEffort);
    }
}
