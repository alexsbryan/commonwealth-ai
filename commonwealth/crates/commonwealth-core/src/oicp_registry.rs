use serde::{Deserialize, Serialize};

use crate::oicp::CapabilityProfile;
use crate::{Error, Result};

/// A community-maintained OICP profile for a specific model+quantization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OicpProfileEntry {
    pub model_repo: String,
    pub quantization: String,
    pub profile_id: String,
    pub capabilities: CapabilityProfile,
    pub context_tokens: u32,
    #[serde(default)]
    pub notes: Option<String>,
}

/// A collection of OICP profiles (the community registry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OicpProfileRegistry {
    pub profiles: Vec<OicpProfileEntry>,
}

/// Parse a single OICP profile from TOML.
pub fn parse_profile(toml_str: &str) -> Result<OicpProfileEntry> {
    toml::from_str(toml_str)
        .map_err(|e| Error::Config(format!("failed to parse OICP profile: {e}")))
}

/// Parse a registry (multiple profiles) from TOML.
pub fn parse_registry(toml_str: &str) -> Result<OicpProfileRegistry> {
    toml::from_str(toml_str)
        .map_err(|e| Error::Config(format!("failed to parse OICP registry: {e}")))
}

/// Look up a profile by model repo and quantization.
pub fn lookup_profile<'a>(
    registry: &'a OicpProfileRegistry,
    repo: &str,
    quant: &str,
) -> Option<&'a OicpProfileEntry> {
    registry
        .profiles
        .iter()
        .find(|p| p.model_repo == repo && p.quantization == quant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oicp::Capability;

    #[test]
    fn parse_single_profile() {
        let toml = r#"
model_repo = "Qwen/Qwen3-Coder-30B-GGUF"
quantization = "Q4_K_M"
profile_id = "qwen/qwen3-coder-30b-Q4_K_M"
context_tokens = 32768
notes = "Strong coding model"

[capabilities]
code = 4
instruction = 3
general = 2
math = 3
"#;
        let profile = parse_profile(toml).unwrap();
        assert_eq!(profile.model_repo, "Qwen/Qwen3-Coder-30B-GGUF");
        assert_eq!(profile.quantization, "Q4_K_M");
        assert_eq!(profile.capabilities.get(Capability::Code), 4);
        assert_eq!(profile.capabilities.get(Capability::General), 2);
        assert_eq!(profile.context_tokens, 32768);
        assert_eq!(profile.notes.as_deref(), Some("Strong coding model"));
    }

    #[test]
    fn parse_multi_profile_registry() {
        let toml = r#"
[[profiles]]
model_repo = "Qwen/Qwen3-Coder-30B-GGUF"
quantization = "Q4_K_M"
profile_id = "qwen/qwen3-coder-30b-Q4_K_M"
context_tokens = 32768

[profiles.capabilities]
code = 4
instruction = 3

[[profiles]]
model_repo = "Qwen/Qwen3-30B-GGUF"
quantization = "Q4_K_M"
profile_id = "qwen/qwen3-30b-Q4_K_M"
context_tokens = 32768

[profiles.capabilities]
general = 3
analysis = 3
creative = 3
"#;
        let registry = parse_registry(toml).unwrap();
        assert_eq!(registry.profiles.len(), 2);
        assert_eq!(registry.profiles[0].capabilities.get(Capability::Code), 4);
        assert_eq!(
            registry.profiles[1].capabilities.get(Capability::General),
            3
        );
    }

    #[test]
    fn lookup_by_repo_and_quant() {
        let toml = r#"
[[profiles]]
model_repo = "Qwen/Qwen3-Coder-30B-GGUF"
quantization = "Q4_K_M"
profile_id = "qwen/qwen3-coder-30b-Q4_K_M"
context_tokens = 32768
[profiles.capabilities]
code = 4

[[profiles]]
model_repo = "Qwen/Qwen3-30B-GGUF"
quantization = "Q8_0"
profile_id = "qwen/qwen3-30b-Q8_0"
context_tokens = 32768
[profiles.capabilities]
general = 3
"#;
        let registry = parse_registry(toml).unwrap();

        let found = lookup_profile(&registry, "Qwen/Qwen3-Coder-30B-GGUF", "Q4_K_M");
        assert!(found.is_some());
        assert_eq!(found.unwrap().capabilities.get(Capability::Code), 4);

        let not_found = lookup_profile(&registry, "Qwen/Qwen3-Coder-30B-GGUF", "Q8_0");
        assert!(not_found.is_none());
    }

    #[test]
    fn profile_serde_roundtrip() {
        let toml_str = r#"
model_repo = "test/model"
quantization = "Q4_K_M"
profile_id = "test/model-Q4_K_M"
context_tokens = 8192
[capabilities]
general = 2
code = 3
"#;
        let profile = parse_profile(toml_str).unwrap();
        let serialized = toml::to_string(&profile).unwrap();
        let back: OicpProfileEntry = toml::from_str(&serialized).unwrap();
        assert_eq!(back.model_repo, profile.model_repo);
        assert_eq!(
            back.capabilities.get(Capability::Code),
            profile.capabilities.get(Capability::Code)
        );
    }
}
