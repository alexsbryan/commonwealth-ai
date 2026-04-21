use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::oicp::{
    Capability, CapabilityProfile, CapabilityRequirements, ContextRequirements,
    InferenceRequirements, ProficiencyLevel, ShardingPrivacy,
};
use crate::types::{Intent, TrustLevel, compute_trust_level};

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
    /// Named evaluation prompts (e.g., "synthesis" → eval prompt).
    #[serde(default)]
    pub evaluation_prompts: HashMap<String, String>,
    /// OICP inference requirements for this skill.
    #[serde(default)]
    pub inference: SkillInferenceConfig,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub signed_by: Option<String>,
    #[serde(default)]
    pub trust_level: TrustLevel,
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
    pub privacy: ShardingPrivacy,
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
    /// Override the default 10% monthly decay rate (e.g., 0.05 for 5%/month).
    pub confidence_decay_per_month: Option<f64>,
    /// Override the default 0.2 prune threshold.
    pub prune_threshold: Option<f64>,
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
    /// Lowest decay rate wins (most conservative — slowest decay).
    pub confidence_decay_per_month: Option<f64>,
    /// Lowest prune threshold wins (keeps memories longer).
    pub prune_threshold: Option<f64>,
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
    #[serde(default)]
    evaluation: EvaluationToml,
}

#[derive(Debug, Deserialize)]
struct SkillMeta {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    signed_by: Option<String>,
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

/// Named evaluation prompts for step-level quality checking.
/// Keys are eval names (e.g., "synthesis"), values are eval prompts.
#[derive(Debug, Default, Deserialize)]
struct EvaluationToml {
    #[serde(flatten)]
    prompts: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct MemoryToml {
    extract_prompt_addendum: Option<String>,
    confidence_decay_per_month: Option<f64>,
    prune_threshold: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct InferenceToml {
    #[serde(default)]
    preferred_capabilities: HashMap<Capability, ProficiencyLevel>,
    #[serde(default)]
    required_capabilities: HashMap<Capability, ProficiencyLevel>,
    min_context_tokens: Option<usize>,
    #[serde(default)]
    privacy: ShardingPrivacy,
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
                confidence_decay_per_month: self.memory.confidence_decay_per_month,
                prune_threshold: self.memory.prune_threshold,
            },
            evaluation_prompts: self.evaluation.prompts,
            inference: SkillInferenceConfig {
                preferred_capabilities: self.inference.preferred_capabilities,
                required_capabilities: self.inference.required_capabilities,
                min_context_tokens: self.inference.min_context_tokens,
                privacy: self.inference.privacy,
            },
            trust_level: compute_trust_level(&self.skill.signature, &self.skill.signed_by),
            signature: self.skill.signature,
            signed_by: self.skill.signed_by,
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
                    if skill.trust_level == TrustLevel::Unsigned {
                        eprintln!("[skills] Loaded: {} v{} (unsigned)", skill.name, skill.version);
                    } else {
                        eprintln!("[skills] Loaded: {} v{}", skill.name, skill.version);
                    }
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

    /// Register a skill by id, **replacing** any existing skill with
    /// the same id. "Later wins" — important because callers chain
    /// registrations from multiple sources (built-in `include_str!`
    /// defaults, workspace dev overlay, user skills dir) and expect
    /// the last-loaded version to be the one that takes effect.
    ///
    /// Previously this was a naive `push`, which meant a skill loaded
    /// twice (e.g. a built-in that also exists in the dev workspace
    /// overlay) produced duplicate ids in `list()`. The Svelte UI's
    /// keyed `{#each (skill.id)}` block then crashed with a
    /// duplicate-key error and bailed mid-render, freezing the panel
    /// on its previous state ("Loading skills…").
    pub fn register(&mut self, skill: Skill) {
        if let Some(existing) = self.skills.iter_mut().find(|s| s.id == skill.id) {
            *existing = skill;
        } else {
            self.skills.push(skill);
        }
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

    /// Return the ids of all registered skills whose inference config
    /// declares `privacy = "local_only"`. Used by the KnowledgeView
    /// conversational acquirer to strictly exclude those skills'
    /// conversations from the shared conversational corpus.
    ///
    /// Iterates `list()` rather than `active_skills()` so a
    /// skill that *can* be activated (and therefore may have
    /// tagged past conversations) is respected even when currently
    /// inactive — the filter is about what belongs in the corpus,
    /// not which skill is talking now.
    pub fn local_only_skill_ids(&self) -> Vec<String> {
        self.skills
            .iter()
            .filter(|s| matches!(s.inference.privacy, ShardingPrivacy::LocalOnly))
            .map(|s| s.id.clone())
            .collect()
    }

    /// Resolve the active skill whose identity should tag a
    /// newly-started conversation. When multiple skills are active,
    /// the most privacy-restrictive one wins (`LocalOnly` >
    /// `MeshAllowed`) so that a conversation started under
    /// `inner-work` + a background skill is still tagged as
    /// `inner-work` and therefore filtered out of the
    /// conversational KnowledgeView. Returns `None` when no skill
    /// is active.
    pub fn primary_skill_id_for_conversation(&self) -> Option<String> {
        let active = self.active_skills();
        if active.is_empty() {
            return None;
        }
        // Prefer LocalOnly skills; fall back to the first active skill.
        active
            .iter()
            .find(|s| matches!(s.inference.privacy, ShardingPrivacy::LocalOnly))
            .map(|s| s.id.clone())
            .or_else(|| active.first().map(|s| s.id.clone()))
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

    /// Merges OICP inference requirements from all active skills into a
    /// single canonical `InferenceRequirements` (OICP §3).
    ///
    /// - Required: takes the maximum level across skills.
    /// - Preferred: takes the maximum level across skills.
    /// - Privacy: LocalOnly wins (most restrictive).
    /// - Context: takes the maximum across skills.
    pub fn inference_requirements(&self) -> InferenceRequirements {
        let mut required: CapabilityProfile = HashMap::new();
        let mut preferred: CapabilityProfile = HashMap::new();
        let mut min_context: Option<usize> = None;
        let mut privacy = ShardingPrivacy::MeshAllowed;

        for skill in self.active_skills() {
            let inf = &skill.inference;

            for (cap, &level) in &inf.required_capabilities {
                let entry = required.entry(*cap).or_insert(0);
                *entry = (*entry).max(level);
            }

            for (cap, &level) in &inf.preferred_capabilities {
                let entry = preferred.entry(*cap).or_insert(0);
                *entry = (*entry).max(level);
            }

            if let Some(tokens) = inf.min_context_tokens {
                min_context = Some(min_context.map_or(tokens, |t: usize| t.max(tokens)));
            }

            if matches!(inf.privacy, ShardingPrivacy::LocalOnly) {
                privacy = ShardingPrivacy::LocalOnly;
            }
        }

        let mut req = InferenceRequirements::new().with_sharding(privacy);

        if !required.is_empty() || !preferred.is_empty() {
            req = req.with_capabilities(CapabilityRequirements {
                required,
                preferred,
            });
        }

        if let Some(tokens) = min_context {
            req = req.with_context(ContextRequirements {
                min_tokens: Some(tokens as u32),
                preferred_tokens: None,
            });
        }

        req
    }

    pub fn memory_rules(&self) -> MergedMemoryConfig {
        let mut merged = MergedMemoryConfig::default();
        for skill in self.active_skills() {
            if let Some(addendum) = &skill.memory_rules.extract_prompt_addendum {
                merged.extraction_addenda.push(addendum.clone());
            }
            if let Some(decay) = skill.memory_rules.confidence_decay_per_month {
                merged.confidence_decay_per_month = Some(
                    merged
                        .confidence_decay_per_month
                        .map_or(decay, |d: f64| d.min(decay)),
                );
            }
            if let Some(threshold) = skill.memory_rules.prune_threshold {
                merged.prune_threshold = Some(
                    merged
                        .prune_threshold
                        .map_or(threshold, |t: f64| t.min(threshold)),
                );
            }
        }
        merged
    }

    pub fn trust_level(&self, skill_id: &str) -> TrustLevel {
        self.skills
            .iter()
            .find(|s| s.id == skill_id)
            .map(|s| s.trust_level)
            .unwrap_or(TrustLevel::Unsigned)
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

    fn skill_with_privacy(id: &str, privacy: ShardingPrivacy) -> Skill {
        let toml = format!(
            r#"
[skill]
id = "{id}"
name = "{id}"
version = "0.1.0"

[inference]
privacy = "{}"
"#,
            match privacy {
                ShardingPrivacy::LocalOnly => "local_only",
                ShardingPrivacy::MeshAllowed => "mesh_allowed",
            }
        );
        parse_skill_toml(&toml).expect("parse test skill")
    }

    #[test]
    fn local_only_skill_ids_filters_by_privacy() {
        let mut reg = SkillRegistry::new();
        reg.register(skill_with_privacy("inner-work", ShardingPrivacy::LocalOnly));
        reg.register(skill_with_privacy(
            "research-analyst",
            ShardingPrivacy::MeshAllowed,
        ));
        reg.register(skill_with_privacy("journal", ShardingPrivacy::LocalOnly));

        let local_only = reg.local_only_skill_ids();
        assert_eq!(local_only.len(), 2);
        assert!(local_only.contains(&"inner-work".to_string()));
        assert!(local_only.contains(&"journal".to_string()));
        assert!(!local_only.contains(&"research-analyst".to_string()));
    }

    #[test]
    fn local_only_skill_ids_includes_inactive_skills() {
        // Regression guard: the filter is "what belongs in the corpus",
        // not "what's talking now". A skill that's been loaded but not
        // activated must still participate in the exclusion.
        let mut reg = SkillRegistry::new();
        reg.register(skill_with_privacy("inner-work", ShardingPrivacy::LocalOnly));
        // Never call activate() — skill is inactive.
        let local_only = reg.local_only_skill_ids();
        assert_eq!(local_only, vec!["inner-work".to_string()]);
    }

    #[test]
    fn primary_skill_id_for_conversation_prefers_local_only() {
        let mut reg = SkillRegistry::new();
        reg.register(skill_with_privacy(
            "research-analyst",
            ShardingPrivacy::MeshAllowed,
        ));
        reg.register(skill_with_privacy("inner-work", ShardingPrivacy::LocalOnly));
        reg.activate("research-analyst");
        reg.activate("inner-work");

        // Both active; LocalOnly wins so the conversation gets tagged
        // as inner-work and therefore filtered out of the shared
        // conversational corpus.
        assert_eq!(
            reg.primary_skill_id_for_conversation().as_deref(),
            Some("inner-work")
        );
    }

    #[test]
    fn primary_skill_id_for_conversation_none_when_nothing_active() {
        let mut reg = SkillRegistry::new();
        reg.register(skill_with_privacy("x", ShardingPrivacy::MeshAllowed));
        // No activate() — active set is empty.
        assert!(reg.primary_skill_id_for_conversation().is_none());
    }

    #[test]
    fn primary_skill_id_for_conversation_falls_back_to_first_active_when_no_local_only() {
        let mut reg = SkillRegistry::new();
        reg.register(skill_with_privacy(
            "research-analyst",
            ShardingPrivacy::MeshAllowed,
        ));
        reg.activate("research-analyst");
        assert_eq!(
            reg.primary_skill_id_for_conversation().as_deref(),
            Some("research-analyst")
        );
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
