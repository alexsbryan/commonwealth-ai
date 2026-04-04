use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::glob::glob_match;
use crate::oicp::{Capability, CapabilityProfile, CapabilityRequirements, LatencyPreference};

/// The built-in default aliases, embedded at compile time.
const DEFAULT_ALIASES_TOML: &str = include_str!("default_aliases.toml");

/// A single model alias: maps glob patterns to inferred OICP requirements.
#[derive(Debug, Clone)]
pub struct ModelAlias {
    pub patterns: Vec<String>,
    /// Hard floor — model must meet these. Empty for soft matching.
    pub inferred_required: CapabilityProfile,
    /// Scoring preferences — used for ranking among satisfying models.
    pub inferred_preferred: CapabilityProfile,
    pub latency: LatencyPreference,
}

/// Table of model aliases. Checked in order — first match wins.
#[derive(Debug, Clone)]
pub struct ModelAliasTable {
    aliases: Vec<ModelAlias>,
}

/// Resolved alias result with synthesized OICP requirements.
#[derive(Debug, Clone)]
pub struct AliasResolution {
    pub requirements: CapabilityRequirements,
    pub latency: LatencyPreference,
}

impl ModelAliasTable {
    pub fn new(aliases: Vec<ModelAlias>) -> Self {
        Self { aliases }
    }

    /// Resolve a model name against the alias table.
    /// Returns synthesized OICP requirements if a pattern matches.
    /// First match wins — order matters.
    pub fn resolve(&self, model_name: &str) -> Option<AliasResolution> {
        for alias in &self.aliases {
            for pattern in &alias.patterns {
                if glob_match(pattern, model_name) {
                    return Some(AliasResolution {
                        requirements: CapabilityRequirements {
                            required: alias.inferred_required.clone(),
                            preferred: alias.inferred_preferred.clone(),
                        },
                        latency: alias.latency,
                    });
                }
            }
        }
        None
    }

    /// Number of aliases in the table.
    pub fn len(&self) -> usize {
        self.aliases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty()
    }

    /// Build the default alias table from the embedded TOML.
    pub fn default_table() -> Self {
        Self::parse_toml(DEFAULT_ALIASES_TOML).expect("built-in default_aliases.toml is invalid")
    }

    /// Parse an alias table from a TOML string.
    pub fn parse_toml(toml_str: &str) -> crate::Result<Self> {
        let file: AliasFile = toml::from_str(toml_str)
            .map_err(|e| crate::Error::Config(format!("failed to parse alias table: {e}")))?;
        Ok(Self::from_config(&file.aliases))
    }

    /// Build from parsed config entries.
    pub fn from_config(entries: &[ModelAliasConfig]) -> Self {
        let aliases = entries
            .iter()
            .map(|entry| {
                let mut required = CapabilityProfile::new();
                for (cap_str, &level) in &entry.inferred_required {
                    if let Some(cap) = parse_capability(cap_str) {
                        required.set(cap, level);
                    }
                }

                let mut preferred = CapabilityProfile::new();
                for (cap_str, &level) in &entry.inferred_preferred {
                    if let Some(cap) = parse_capability(cap_str) {
                        preferred.set(cap, level);
                    }
                }

                let latency = match entry.latency.as_deref() {
                    Some("interactive") => LatencyPreference::Interactive,
                    Some("throughput") => LatencyPreference::Throughput,
                    Some("background") => LatencyPreference::Background,
                    _ => LatencyPreference::BestEffort,
                };

                ModelAlias {
                    patterns: entry.patterns.clone(),
                    inferred_required: required,
                    inferred_preferred: preferred,
                    latency,
                }
            })
            .collect();

        Self::new(aliases)
    }

    /// Merge another table's aliases after this one.
    /// The original aliases take priority (checked first).
    pub fn extend(&mut self, other: &ModelAliasTable) {
        self.aliases.extend(other.aliases.iter().cloned());
    }
}

impl Default for ModelAliasTable {
    fn default() -> Self {
        Self::default_table()
    }
}

// ─── TOML deserialization types ─────────────────────────────────

/// Top-level structure of the aliases TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AliasFile {
    #[serde(default)]
    aliases: Vec<ModelAliasConfig>,
}

/// A single alias entry as it appears in TOML config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAliasConfig {
    pub patterns: Vec<String>,
    #[serde(default)]
    pub inferred_required: HashMap<String, u8>,
    #[serde(default)]
    pub inferred_preferred: HashMap<String, u8>,
    #[serde(default)]
    pub latency: Option<String>,
}

fn parse_capability(s: &str) -> Option<Capability> {
    match s.to_lowercase().as_str() {
        "general" => Some(Capability::General),
        "code" => Some(Capability::Code),
        "analysis" => Some(Capability::Analysis),
        "math" => Some(Capability::Math),
        "creative" => Some(Capability::Creative),
        "instruction" => Some(Capability::Instruction),
        "multilingual" => Some(Capability::Multilingual),
        "vision" => Some(Capability::Vision),
        "long_context" => Some(Capability::LongContext),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_table_parses_from_toml() {
        let table = ModelAliasTable::default_table();
        assert!(
            table.len() >= 10,
            "expected >= 10 aliases, got {}",
            table.len()
        );
    }

    #[test]
    fn omo_codex_resolves_to_coding() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gpt-5.3-codex").unwrap();
        assert!(result.requirements.preferred.get(Capability::Code) >= 3);
        assert_eq!(result.latency, LatencyPreference::Throughput);
    }

    #[test]
    fn omo_opus_resolves_to_general() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("claude-opus-4-6").unwrap();
        assert!(result.requirements.preferred.get(Capability::General) >= 3);
        assert!(result.requirements.preferred.get(Capability::Analysis) >= 3);
        assert_eq!(result.latency, LatencyPreference::Interactive);
    }

    #[test]
    fn omo_mini_resolves_to_lightweight() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gpt-5.4-nano").unwrap();
        assert_eq!(result.requirements.preferred.get(Capability::General), 1);
        assert_eq!(result.latency, LatencyPreference::Interactive);
    }

    #[test]
    fn generic_coder_pattern() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("deepseek-coder-v3").unwrap();
        assert!(result.requirements.preferred.get(Capability::Code) >= 3);
    }

    #[test]
    fn no_match_returns_none() {
        let table = ModelAliasTable::default_table();
        assert!(table.resolve("totally-unknown-model-xyz").is_none());
    }

    #[test]
    fn first_match_wins() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gpt-5.3-codex").unwrap();
        // The first match (OmO Hephaestus) has instruction: 3.
        assert!(result.requirements.preferred.get(Capability::Instruction) >= 3);
    }

    #[test]
    fn soft_matching_no_required() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gpt-5.3-codex").unwrap();
        assert!(
            result.requirements.required.0.is_empty(),
            "default aliases should use soft matching (no required)"
        );
    }

    #[test]
    fn custom_toml_parsing() {
        let toml = r#"
[[aliases]]
patterns = ["my-custom-model*"]
latency = "throughput"
[aliases.inferred_preferred]
code = 4
math = 3
"#;
        let table = ModelAliasTable::parse_toml(toml).unwrap();
        let result = table.resolve("my-custom-model-v2").unwrap();
        assert_eq!(result.requirements.preferred.get(Capability::Code), 4);
        assert_eq!(result.requirements.preferred.get(Capability::Math), 3);
    }

    #[test]
    fn custom_toml_with_required() {
        let toml = r#"
[[aliases]]
patterns = ["strict-coder*"]
latency = "throughput"
[aliases.inferred_required]
code = 3
[aliases.inferred_preferred]
code = 4
instruction = 3
"#;
        let table = ModelAliasTable::parse_toml(toml).unwrap();
        let result = table.resolve("strict-coder-v1").unwrap();
        assert_eq!(result.requirements.required.get(Capability::Code), 3);
        assert_eq!(result.requirements.preferred.get(Capability::Code), 4);
    }

    #[test]
    fn extend_merges_tables() {
        let mut base = ModelAliasTable::default_table();
        let extra_toml = r#"
[[aliases]]
patterns = ["my-special*"]
[aliases.inferred_preferred]
creative = 4
"#;
        let extra = ModelAliasTable::parse_toml(extra_toml).unwrap();
        let base_len = base.len();
        base.extend(&extra);
        assert_eq!(base.len(), base_len + 1);
        assert!(base.resolve("my-special-model").is_some());
        // Original aliases still work.
        assert!(base.resolve("gpt-5.3-codex").is_some());
    }

    #[test]
    fn kimi_resolves() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("kimi-k2-latest").unwrap();
        assert!(result.requirements.preferred.get(Capability::Code) >= 3);
    }

    #[test]
    fn gemini_flash_resolves_to_fast() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gemini-2.5-flash").unwrap();
        assert_eq!(result.latency, LatencyPreference::Interactive);
    }
}
