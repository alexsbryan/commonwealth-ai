use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::oicp::{
    Capability, CapabilityProfile, InferenceRequirements, LatencyPreference, PrivacyPreference,
    ProficiencyLevel,
};
use crate::types::Intent;

// ─── Skill Definition ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub routing: RoutingHints,
    pub planner_templates: Vec<PlanTemplate>,
    pub tool_config: ToolPreferences,
    pub prompts: PromptOverrides,
    pub memory_rules: MemoryConfig,
    /// OICP inference requirements for this skill.
    #[serde(default)]
    pub inference: SkillInferenceConfig,
}

/// OICP inference configuration declared in a skill manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillInferenceConfig {
    #[serde(default)]
    pub preferred_capabilities: HashMap<Capability, ProficiencyLevel>,
    #[serde(default)]
    pub required_capabilities: HashMap<Capability, ProficiencyLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_context_tokens: Option<usize>,
    #[serde(default)]
    pub privacy: PrivacyPreference,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingHints {
    pub trigger_phrases: Vec<String>,
    pub default_intent: Option<String>,
    pub min_confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTemplate {
    pub name: String,
    pub trigger: String,
    pub steps: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolPreferences {
    pub required: Vec<String>,
    pub optional: Vec<String>,
    #[serde(default)]
    pub tool_settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptOverrides {
    pub synthesis: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub extract_prompt_addendum: Option<String>,
}

// ─── Merged Results ────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MergedRoutingHints {
    pub trigger_phrases: Vec<(String, String)>, // (phrase, skill_id)
    pub min_confidence: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MergedMemoryConfig {
    pub extraction_addenda: Vec<String>,
}

// ─── TOML Loading ──────────────────────────────────────────────

/// TOML structure for skill.toml files.
/// Maps the TOML layout to our internal Skill struct.
#[derive(Debug, Deserialize)]
struct SkillToml {
    skill: SkillMeta,
    #[serde(default)]
    routing: RoutingToml,
    #[serde(default)]
    planner: PlannerToml,
    #[serde(default)]
    tools: ToolsToml,
    #[serde(default)]
    prompts: PromptsToml,
    #[serde(default)]
    memory: MemoryToml,
    #[serde(default)]
    inference: InferenceToml,
}

#[derive(Debug, Deserialize)]
struct SkillMeta {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Default, Deserialize)]
struct RoutingToml {
    #[serde(default)]
    trigger_phrases: Vec<String>,
    default_intent: Option<String>,
    min_confidence: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct PlannerToml {
    #[serde(default)]
    templates: Vec<PlanTemplateToml>,
}

#[derive(Debug, Deserialize)]
struct PlanTemplateToml {
    name: String,
    trigger: String,
    steps: String,
}

#[derive(Debug, Default, Deserialize)]
struct ToolsToml {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
    // Remaining keys are tool-specific settings (e.g., [tools.web_search]).
    #[serde(flatten)]
    settings: HashMap<String, toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct PromptsToml {
    synthesis: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct MemoryToml {
    extract_prompt_addendum: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct InferenceToml {
    #[serde(default)]
    preferred_capabilities: HashMap<Capability, ProficiencyLevel>,
    #[serde(default)]
    required_capabilities: HashMap<Capability, ProficiencyLevel>,
    min_context_tokens: Option<usize>,
    #[serde(default)]
    privacy: PrivacyPreference,
}

impl SkillToml {
    fn into_skill(self) -> Skill {
        // Convert tool settings from toml::Value to serde_json::Value.
        let tool_settings: HashMap<String, serde_json::Value> = self
            .tools
            .settings
            .into_iter()
            .filter_map(|(k, v)| {
                // Skip the "required" and "optional" keys which are already parsed.
                if k == "required" || k == "optional" {
                    return None;
                }
                let json_str = serde_json::to_string(&v).ok()?;
                let json_val: serde_json::Value = serde_json::from_str(&json_str).ok()?;
                Some((k, json_val))
            })
            .collect();

        Skill {
            id: self.skill.id,
            name: self.skill.name,
            version: self.skill.version,
            description: self.skill.description,
            routing: RoutingHints {
                trigger_phrases: self.routing.trigger_phrases,
                default_intent: self.routing.default_intent,
                min_confidence: self.routing.min_confidence,
            },
            planner_templates: self
                .planner
                .templates
                .into_iter()
                .map(|t| PlanTemplate {
                    name: t.name,
                    trigger: t.trigger,
                    steps: t.steps,
                })
                .collect(),
            tool_config: ToolPreferences {
                required: self.tools.required,
                optional: self.tools.optional,
                tool_settings,
            },
            prompts: PromptOverrides {
                synthesis: self.prompts.synthesis,
            },
            memory_rules: MemoryConfig {
                extract_prompt_addendum: self.memory.extract_prompt_addendum,
            },
            inference: SkillInferenceConfig {
                preferred_capabilities: self.inference.preferred_capabilities,
                required_capabilities: self.inference.required_capabilities,
                min_context_tokens: self.inference.min_context_tokens,
                privacy: self.inference.privacy,
            },
        }
    }
}

/// Parse a skill from a TOML string.
pub fn parse_skill_toml(toml_str: &str) -> Option<Skill> {
    match toml::from_str::<SkillToml>(toml_str) {
        Ok(skill_toml) => Some(skill_toml.into_skill()),
        Err(e) => {
            eprintln!("[skills] Failed to parse skill TOML: {e}");
            None
        }
    }
}

/// Load all skills from a directory. Each subdirectory should contain a skill.toml.
pub fn load_from_directory(dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("[skills] Could not read skills directory {}: {e}", dir.display());
            return skills;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let toml_path = path.join("skill.toml");
        if !toml_path.exists() {
            continue;
        }

        match std::fs::read_to_string(&toml_path) {
            Ok(content) => {
                if let Some(skill) = parse_skill_toml(&content) {
                    eprintln!("[skills] Loaded: {} v{}", skill.name, skill.version);
                    skills.push(skill);
                } else {
                    eprintln!(
                        "[skills] Skipping malformed skill: {}",
                        toml_path.display()
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "[skills] Could not read {}: {e}",
                    toml_path.display()
                );
            }
        }
    }

    skills
}

// ─── SkillRegistry ─────────────────────────────────────────────

pub struct SkillRegistry {
    skills: Vec<Skill>,
    active: HashSet<String>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
            active: HashSet::new(),
        }
    }

    pub fn register(&mut self, skill: Skill) {
        self.skills.push(skill);
    }

    /// Load skills from a directory and register them all.
    pub fn load_and_register(&mut self, dir: &Path) {
        for skill in load_from_directory(dir) {
            self.register(skill);
        }
    }

    pub fn activate(&mut self, skill_id: &str) {
        self.active.insert(skill_id.to_string());
    }

    pub fn activate_all(&mut self) {
        for skill in &self.skills {
            self.active.insert(skill.id.clone());
        }
    }

    pub fn deactivate(&mut self, skill_id: &str) {
        self.active.remove(skill_id);
    }

    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    pub fn active_skills(&self) -> Vec<&Skill> {
        self.skills
            .iter()
            .filter(|s| self.active.contains(&s.id))
            .collect()
    }

    pub fn routing_hints(&self) -> MergedRoutingHints {
        let mut merged = MergedRoutingHints::default();
        for skill in self.active_skills() {
            for phrase in &skill.routing.trigger_phrases {
                merged
                    .trigger_phrases
                    .push((phrase.clone(), skill.id.clone()));
            }
            if let Some(conf) = skill.routing.min_confidence {
                merged.min_confidence = Some(
                    merged
                        .min_confidence
                        .map_or(conf, |existing: f64| existing.max(conf)),
                );
            }
        }
        merged
    }

    pub fn planner_templates(&self, _intent: &Intent) -> Vec<&PlanTemplate> {
        self.active_skills()
            .iter()
            .flat_map(|s| s.planner_templates.iter())
            .collect()
    }

    pub fn prompt_overrides(&self, _intent: &Intent) -> Option<String> {
        let overrides: Vec<&str> = self
            .active_skills()
            .iter()
            .filter_map(|s| s.prompts.synthesis.as_deref())
            .collect();

        if overrides.is_empty() {
            None
        } else {
            Some(overrides.join("\n\n---\n\n"))
        }
    }

    /// Merges OICP inference requirements from all active skills.
    /// Required: takes the maximum level across skills.
    /// Preferred: takes the maximum level across skills.
    /// Privacy: LocalOnly wins (most restrictive).
    /// Context: takes the maximum across skills.
    pub fn inference_requirements(&self) -> InferenceRequirements {
        let mut required: CapabilityProfile = HashMap::new();
        let mut preferred: CapabilityProfile = HashMap::new();
        let mut min_context: Option<usize> = None;
        let mut privacy = PrivacyPreference::MeshAllowed;

        for skill in self.active_skills() {
            let inf = &skill.inference;

            for (cap, &level) in &inf.required_capabilities {
                let entry = required.entry(cap.clone()).or_insert(0);
                *entry = (*entry).max(level);
            }

            for (cap, &level) in &inf.preferred_capabilities {
                let entry = preferred.entry(cap.clone()).or_insert(0);
                *entry = (*entry).max(level);
            }

            if let Some(tokens) = inf.min_context_tokens {
                min_context = Some(min_context.map_or(tokens, |t: usize| t.max(tokens)));
            }

            if matches!(inf.privacy, PrivacyPreference::LocalOnly) {
                privacy = PrivacyPreference::LocalOnly;
            }
        }

        InferenceRequirements {
            required,
            preferred,
            min_context_tokens: min_context,
            latency: LatencyPreference::BestEffort,
            privacy,
        }
    }

    pub fn memory_rules(&self) -> MergedMemoryConfig {
        let mut merged = MergedMemoryConfig::default();
        for skill in self.active_skills() {
            if let Some(addendum) = &skill.memory_rules.extract_prompt_addendum {
                merged.extraction_addenda.push(addendum.clone());
            }
        }
        merged
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_skill_toml() {
        let toml = r#"
[skill]
id = "research-analyst"
name = "Research & Analysis"
version = "0.1.0"
description = "Deep multi-source research with citations."

[routing]
trigger_phrases = ["research", "investigate"]
default_intent = "ComplexTask"
min_confidence = 0.75

[[planner.templates]]
name = "multi_source_research"
trigger = "User wants information synthesized from multiple sources"
steps = """
1. Decompose the question into sub-queries.
2. Execute sub-queries via web_search.
3. Synthesize into a cited response.
"""

[tools]
required = ["web_search"]
optional = ["knowledge"]

[prompts]
synthesis = "You are a research analyst. Cite sources."

[memory]
extract_prompt_addendum = "Extract topics researched."
"#;

        let skill = parse_skill_toml(toml).unwrap();
        assert_eq!(skill.id, "research-analyst");
        assert_eq!(skill.name, "Research & Analysis");
        assert_eq!(skill.routing.trigger_phrases.len(), 2);
        assert_eq!(skill.routing.min_confidence, Some(0.75));
        assert_eq!(skill.planner_templates.len(), 1);
        assert_eq!(skill.planner_templates[0].name, "multi_source_research");
        assert_eq!(skill.tool_config.required, vec!["web_search"]);
        assert!(skill.prompts.synthesis.is_some());
        assert!(skill.memory_rules.extract_prompt_addendum.is_some());
    }

    #[test]
    fn parse_minimal_skill_toml() {
        let toml = r#"
[skill]
id = "minimal"
name = "Minimal Skill"
version = "0.1.0"
"#;

        let skill = parse_skill_toml(toml).unwrap();
        assert_eq!(skill.id, "minimal");
        assert!(skill.routing.trigger_phrases.is_empty());
        assert!(skill.planner_templates.is_empty());
        assert!(skill.prompts.synthesis.is_none());
    }

    #[test]
    fn parse_malformed_toml_returns_none() {
        let toml = "this is not valid toml {{{{";
        assert!(parse_skill_toml(toml).is_none());
    }

    #[test]
    fn parse_missing_required_fields_returns_none() {
        // Missing [skill] section entirely.
        let toml = r#"
[routing]
trigger_phrases = ["test"]
"#;
        assert!(parse_skill_toml(toml).is_none());
    }

    #[test]
    fn skill_with_tool_settings() {
        let toml = r#"
[skill]
id = "test"
name = "Test"
version = "0.1.0"

[tools]
required = ["web_search"]

[tools.web_search]
min_sub_queries = 3
max_sub_queries = 5
"#;

        let skill = parse_skill_toml(toml).unwrap();
        assert!(skill.tool_config.tool_settings.contains_key("web_search"));
    }
}
