use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::glob::glob_match;
use crate::oicp::{
    infer_hint_from_profile, Capability, CapabilityHint, CapabilityProfile,
    LatencyClass,
};

/// The built-in default aliases, embedded at compile time.
const DEFAULT_ALIASES_TOML: &str = include_str!("default_aliases.toml");

/// A single model alias: maps glob patterns to inferred v0.3 routing
/// properties (hint + latency class). The TOML source format still
/// uses v0.2 capability-profile vocabulary for author convenience —
/// those are collapsed to a single hint via
/// [`infer_hint_from_profile`] at parse time.
#[derive(Debug, Clone)]
pub struct ModelAlias {
    pub patterns: Vec<String>,
    /// v0.3 capability hint synthesized from the TOML's
    /// `inferred_preferred` profile.
    pub hint: CapabilityHint,
    /// v0.3 latency class synthesized from the TOML's `latency`
    /// string.
    pub latency_class: LatencyClass,
    /// Preserved for diagnostics + PR-D affinity derivation.
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
                let mut preferred = CapabilityProfile::new();
                for (cap_str, &level) in &entry.inferred_preferred {
                    if let Some(cap) = parse_capability(cap_str) {
                        preferred.insert(cap, level);
                    }
                }

                // Map the TOML's legacy latency vocabulary into v0.3
                // classes. Interactive → Fast (UI-speed);
                // throughput / background → Extended (user accepts
                // higher TTFT); best_effort / unset → Normal.
                let latency_class = match entry.latency.as_deref() {
                    Some("interactive") => LatencyClass::Fast,
                    Some("throughput") | Some("background") => {
                        LatencyClass::Extended
                    }
                    _ => LatencyClass::Normal,
                };

                // Aliases are routing hints, not model profiles —
                // an alias named `codex` saying "code=3" is
                // communicating "this is a coding alias" regardless
                // of the numeric proficiency. Use a more permissive
                // rule than `infer_hint_from_profile`: if `code` is
                // the highest-proficiency entry in the preferred
                // profile (and non-zero), route as code; otherwise
                // general.
                let code_level = preferred
                    .get(&Capability::Code)
                    .copied()
                    .unwrap_or(0);
                let max_level = preferred
                    .values()
                    .copied()
                    .max()
                    .unwrap_or(0);
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

/// A single alias entry as it appears in TOML config. The legacy
/// `inferred_required` field is still deserialized so existing TOML
/// files don't error, but it's no longer used in v0.3 routing —
/// the scheduler ranks on claim hard gates, not alias-declared
/// proficiency floors.
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
    use crate::oicp;

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
    fn omo_codex_resolves_to_code_hint() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gpt-5.3-codex").unwrap();
        assert!(
            oicp::proficiency(&result.inferred_preferred, Capability::Code) >= 3
        );
        // The alias's "throughput" latency maps to Extended in v0.3.
        assert_eq!(result.latency_class, LatencyClass::Extended);
    }

    #[test]
    fn omo_opus_resolves_to_general() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("claude-opus-4-6").unwrap();
        assert_eq!(result.hint, CapabilityHint::general());
        assert_eq!(result.latency_class, LatencyClass::Fast);
    }

    #[test]
    fn omo_mini_resolves_to_lightweight() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gpt-5.4-nano").unwrap();
        assert_eq!(
            oicp::proficiency(&result.inferred_preferred, Capability::General),
            1
        );
        assert_eq!(result.latency_class, LatencyClass::Fast);
    }

    #[test]
    fn generic_coder_pattern() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("deepseek-coder-v3").unwrap();
        assert!(
            oicp::proficiency(&result.inferred_preferred, Capability::Code) >= 3
        );
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
        assert!(
            oicp::proficiency(&result.inferred_preferred, Capability::Instruction)
                >= 3
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
        assert_eq!(
            oicp::proficiency(&result.inferred_preferred, Capability::Code),
            4
        );
        assert_eq!(
            oicp::proficiency(&result.inferred_preferred, Capability::Math),
            3
        );
        // code:4 with no general → code hint per infer_hint_from_profile.
        assert_eq!(result.hint, CapabilityHint::code());
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
        assert!(base.resolve("gpt-5.3-codex").is_some());
    }

    #[test]
    fn kimi_resolves() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("kimi-k2-latest").unwrap();
        assert!(
            oicp::proficiency(&result.inferred_preferred, Capability::Code) >= 3
        );
    }

    #[test]
    fn gemini_flash_resolves_to_fast_latency() {
        let table = ModelAliasTable::default_table();
        let result = table.resolve("gemini-2.5-flash").unwrap();
        assert_eq!(result.latency_class, LatencyClass::Fast);
    }
}
