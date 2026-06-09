// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::oicp::{
    infer_hint_from_profile, Capability, CapabilityHint, CapabilityProfile, LatencyClass,
};
use commonwealth_core::glob::glob_match;

/// The built-in default aliases, embedded at compile time.
/// Re-uses the same TOML file from commonwealth-core.
const DEFAULT_ALIASES_TOML: &str = include_str!("../../commonwealth-core/src/default_aliases.toml");

/// A single model alias: maps glob patterns to inferred v0.3 routing
/// properties (hint + latency class). The TOML source format still
/// uses v0.2 capability-profile vocabulary for author convenience —
/// those collapse to a single hint at parse time.
#[derive(Debug, Clone)]
pub struct ModelAlias {
    pub patterns: Vec<String>,
    pub hint: CapabilityHint,
    pub latency_class: LatencyClass,
    pub inferred_preferred: CapabilityProfile,
}

/// Table of model aliases. Checked in order — first match wins.
#[derive(Debug, Clone)]
pub struct ModelAliasTable {
    aliases: Vec<ModelAlias>,
}

/// Resolved alias result with synthesized v0.3 routing properties.
#[derive(Debug, Clone)]
pub struct AliasResolution {
    pub hint: CapabilityHint,
    pub latency_class: LatencyClass,
    pub inferred_preferred: CapabilityProfile,
}

impl ModelAliasTable {
    pub fn new(aliases: Vec<ModelAlias>) -> Self {
        Self { aliases }
    }

    /// Resolve a model name against the alias table.
    /// First match wins — order matters.
    pub fn resolve(&self, model_name: &str) -> Option<AliasResolution> {
        for alias in &self.aliases {
            for pattern in &alias.patterns {
                if glob_match(pattern, model_name) {
                    return Some(AliasResolution {
                        hint: alias.hint.clone(),
                        latency_class: alias.latency_class,
                        inferred_preferred: alias.inferred_preferred.clone(),
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
        let file: AliasFile = toml::from_str(toml_str).map_err(|e| {
            commonwealth_core::Error::Config(format!("failed to parse alias table: {e}"))
        })?;
        Ok(Self::from_config(&file.aliases))
    }

    pub fn from_config(entries: &[ModelAliasConfig]) -> Self {
        let aliases = entries
            .iter()
            .map(|entry| {
                let mut preferred = CapabilityProfile::new();
                for (cap_str, &level) in &entry.inferred_preferred {
                    if let Some(cap) = parse_capability(cap_str) {
                        preferred.insert(cap, level);
                    }
                }
                let latency_class = match entry.latency.as_deref() {
                    Some("interactive") => LatencyClass::Fast,
                    Some("throughput") | Some("background") => LatencyClass::Extended,
                    _ => LatencyClass::Normal,
                };
                // Aliases are routing hints, not model profiles —
                // an alias named `codex` saying "code=3" means
                // "this is a coding alias" regardless of the
                // numeric proficiency. Permissive: any preferred
                // profile where `code` is the highest-proficiency
                // entry routes as code; otherwise fall through to
                // the stricter model-profile inference.
                let code_level = preferred.get(&Capability::Code).copied().unwrap_or(0);
                let max_level = preferred.values().copied().max().unwrap_or(0);
                let hint = if code_level > 0 && code_level >= max_level {
                    CapabilityHint::code()
                } else {
                    infer_hint_from_profile(&preferred)
                };
                ModelAlias {
                    patterns: entry.patterns.clone(),
                    hint,
                    latency_class,
                    inferred_preferred: preferred,
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

/// A single alias entry as it appears in TOML config. The legacy
/// `inferred_required` field is still deserialized so existing TOML
/// files don't error, but it's no longer used in v0.3 routing.
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
