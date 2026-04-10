use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use commonwealth_core::glob::glob_match;
use crate::oicp::{Capability, CapabilityProfile, CapabilityRequirements, LatencyPreference};

/// The built-in default aliases, embedded at compile time.
/// Re-uses the same TOML file from commonwealth-core.
const DEFAULT_ALIASES_TOML: &str =
    include_str!("../../commonwealth-core/src/default_aliases.toml");

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

    pub fn len(&self) -> usize {
        self.aliases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty()
    }

    pub fn default_table() -> Self {
        Self::parse_toml(DEFAULT_ALIASES_TOML).expect("built-in default_aliases.toml is invalid")
    }

    pub fn parse_toml(toml_str: &str) -> commonwealth_core::Result<Self> {
        let file: AliasFile = toml::from_str(toml_str)
            .map_err(|e| commonwealth_core::Error::Config(format!("failed to parse alias table: {e}")))?;
        Ok(Self::from_config(&file.aliases))
    }

    pub fn from_config(entries: &[ModelAliasConfig]) -> Self {
        let aliases = entries
            .iter()
            .map(|entry| {
                let mut required = CapabilityProfile::new();
                for (cap_str, &level) in &entry.inferred_required {
                    if let Some(cap) = parse_capability(cap_str) {
                        required.insert(cap, level);
                    }
                }
                let mut preferred = CapabilityProfile::new();
                for (cap_str, &level) in &entry.inferred_preferred {
                    if let Some(cap) = parse_capability(cap_str) {
                        preferred.insert(cap, level);
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

    pub fn extend(&mut self, other: &ModelAliasTable) {
        self.aliases.extend(other.aliases.iter().cloned());
    }
}

impl Default for ModelAliasTable {
    fn default() -> Self {
        Self::default_table()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AliasFile {
    #[serde(default)]
    aliases: Vec<ModelAliasConfig>,
}

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
