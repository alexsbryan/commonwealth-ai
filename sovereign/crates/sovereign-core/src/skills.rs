use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::oicp::{
    Capability, CapabilityHint, InferenceRequirements, LatencyClass,
    ProficiencyLevel, ShardingPrivacy,
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
    /// Plan templates the planner injects as a hint when this skill
    /// is active and the user's goal matches a template's trigger.
    /// Consumed by `planner.rs::plan` for ComplexTask flows.
    pub planner_templates: Vec<PlanTemplate>,
    pub tool_config: ToolPreferences,
    /// Synthesis prompt overrides — concatenated by
    /// `SkillRegistry::prompt_overrides()` and prepended to the
    /// executor's ReasonWithTools / reasoning step system message
    /// for ComplexTask flows. Read at `executor.rs:461,1020`.
    pub prompts: PromptOverrides,
    pub memory_rules: MemoryConfig,
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

/// OICP v0.3 inference configuration declared in a skill manifest.
///
/// Skills declare the **shape** of inference work they do in
/// v0.3-native terms: a capability hint (`general` / `code` /
/// `x:<extension>`), a latency class, and structural envelopes
/// (context / output tokens). Legacy `preferred_capabilities` and
/// `required_capabilities` maps are retained as optional hints so
/// existing skill files don't break while migration is in flight —
/// they're no longer threaded through the scheduler but may still be
/// surfaced in docs or used by advertisers to derive claim affinity
/// heuristics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillInferenceConfig {
    /// v0.3 capability hint (e.g., `"general"`, `"code"`,
    /// `"x:prose"`). Parsed via [`CapabilityHint::parse`] at merge
    /// time; invalid hints log a warning and are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_hint: Option<String>,
    /// v0.3 latency class this skill's work needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_class: Option<LatencyClass>,
    /// Minimum context length the skill's work may need. Translated
    /// to `InferenceRequirements.context_tokens` so scheduler hard
    /// gates can eliminate undersized claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_context_tokens: Option<usize>,
    /// Maximum output length the skill's work may produce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
    /// Whether the skill tolerates cross-node routing.
    #[serde(default)]
    pub privacy: ShardingPrivacy,
    /// Legacy: capability proficiencies the skill prefers. Preserved
    /// for skills that haven't migrated to `capability_hint`; not
    /// threaded through the scheduler.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub preferred_capabilities: HashMap<Capability, ProficiencyLevel>,
    /// Legacy: capability proficiencies the skill requires. Same
    /// rationale as `preferred_capabilities`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub required_capabilities: HashMap<Capability, ProficiencyLevel>,
    /// Voice register the skill operates in. `Factual` (default)
    /// keeps the established `PRIMARY_BASE_SYSTEM_PROMPT` /
    /// `KNOWLEDGE_SYNTHESIS_SYSTEM` epistemic contract. `Relational`
    /// opts the skill into the situated/glass-box voice contract
    /// (specific uncertainty, three epistemic registers for user
    /// history, banned generic disclaimers, willingness to disagree
    /// kindly). Selected per-skill; non-relational skills are
    /// untouched.
    #[serde(default)]
    pub register: SkillRegister,
}

/// Voice register a skill operates in. Selects which base system
/// prompt the runtime prepends. Default `Factual` matches the
/// pre-existing behavior; `Relational` activates the glass-box
/// voice contract for situated, personal, or reflective work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillRegister {
    #[default]
    Factual,
    Relational,
}

// `RoutingHints` retired — skill-keyed router hints were the
// load-bearing input to the Pass 1 prompt's "Active skill hints"
// splice block; that splice was removed when the skills-as-menu
// UI retired. The wisdom in the retired skills' `trigger_phrases`
// lists migrated into the router's embed-exemplar bank
// (`sovereign/router/exemplars.toml`); see the migration commit
// for the audit. The surviving modes (inner-work, recipe-author)
// do not need router hints because the user explicitly enters
// their surfaces.

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

// `MergedRoutingHints` retired alongside `RoutingHints` + the
// trigger-phrase splice in router.rs. No production consumers
// remain.

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
///
/// Serde does NOT `deny_unknown_fields`, so legacy mode TOMLs that
/// still carry `[routing]` or `[evaluation]` blocks load cleanly
/// (the blocks are silently ignored). New mode TOMLs should omit
/// those sections.
#[derive(Debug, Deserialize)]
struct SkillToml {
    skill: SkillMeta,
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
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    signed_by: Option<String>,
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
    confidence_decay_per_month: Option<f64>,
    prune_threshold: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct InferenceToml {
    #[serde(default)]
    capability_hint: Option<String>,
    #[serde(default)]
    latency_class: Option<LatencyClass>,
    #[serde(default)]
    min_context_tokens: Option<usize>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
    #[serde(default)]
    privacy: ShardingPrivacy,
    // Legacy fields — skills that haven't migrated still deserialize
    // cleanly. Merged into InferenceRequirements only as diagnostics;
    // the scheduler no longer consumes them.
    #[serde(default)]
    preferred_capabilities: HashMap<Capability, ProficiencyLevel>,
    #[serde(default)]
    required_capabilities: HashMap<Capability, ProficiencyLevel>,
    #[serde(default)]
    register: SkillRegister,
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
            inference: SkillInferenceConfig {
                capability_hint: self.inference.capability_hint,
                latency_class: self.inference.latency_class,
                min_context_tokens: self.inference.min_context_tokens,
                max_output_tokens: self.inference.max_output_tokens,
                privacy: self.inference.privacy,
                preferred_capabilities: self.inference.preferred_capabilities,
                required_capabilities: self.inference.required_capabilities,
                register: self.inference.register,
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

    /// Look up a registered skill by id. Returns `None` when no skill
    /// with that id is registered.
    pub fn skill_by_id(&self, id: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.id == id)
    }

    /// Resolve the voice register of the currently-primary skill.
    /// Returns `SkillRegister::Factual` when no skill is active or
    /// the resolved skill doesn't override the default — preserving
    /// pre-existing behavior for non-relational sessions.
    pub fn primary_skill_register(&self) -> SkillRegister {
        self.primary_skill_id_for_conversation()
            .as_deref()
            .and_then(|id| self.skill_by_id(id))
            .map(|s| s.inference.register)
            .unwrap_or_default()
    }

    // `routing_hints()` retired alongside `MergedRoutingHints` +
    // `RoutingHints`. The skill-keyed trigger-phrase splice in the
    // router's Pass 1 prompt was removed when the skills-as-menu UI
    // was retired; the trigger-phrase wisdom migrated to the embed-
    // exemplar bank.

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
    /// Merge active skills' inference declarations into a single
    /// [`InferenceRequirements`].
    ///
    /// Merge rules:
    /// - Privacy: `LocalOnly` wins (most restrictive).
    /// - `capability_hint`: the first active skill that declares a
    ///   valid hint wins. Skills with no declared hint contribute
    ///   nothing — they accept whatever routing the intent-level
    ///   default provides.
    /// - `latency_class`: first-declared wins, same rationale.
    /// - `min_context_tokens` / `max_output_tokens`: take the
    ///   maximum across skills. The scheduler uses these as hard
    ///   feasibility gates, so the most demanding skill sets the
    ///   floor.
    pub fn inference_requirements(&self) -> InferenceRequirements {
        let mut hint: Option<CapabilityHint> = None;
        let mut latency_class: Option<LatencyClass> = None;
        let mut min_context: Option<u32> = None;
        let mut max_output: Option<u32> = None;
        let mut privacy = ShardingPrivacy::MeshAllowed;

        for skill in self.active_skills() {
            let inf = &skill.inference;

            if hint.is_none() {
                if let Some(raw) = inf.capability_hint.as_deref() {
                    match CapabilityHint::parse(raw) {
                        Ok(h) => hint = Some(h),
                        Err(e) => tracing::warn!(
                            skill = %skill.id,
                            capability_hint = %raw,
                            error = %e,
                            "skill declared invalid capability_hint — ignoring"
                        ),
                    }
                }
            }

            if latency_class.is_none() {
                if let Some(lc) = inf.latency_class {
                    latency_class = Some(lc);
                }
            }

            if let Some(tokens) = inf.min_context_tokens {
                let tokens = tokens as u32;
                min_context = Some(min_context.map_or(tokens, |t| t.max(tokens)));
            }

            if let Some(tokens) = inf.max_output_tokens {
                let tokens = tokens as u32;
                max_output = Some(max_output.map_or(tokens, |t| t.max(tokens)));
            }

            if matches!(inf.privacy, ShardingPrivacy::LocalOnly) {
                privacy = ShardingPrivacy::LocalOnly;
            }
        }

        let mut req = InferenceRequirements::new().with_sharding(privacy);
        if let Some(h) = hint {
            req = req.with_hint(h);
        }
        if let Some(lc) = latency_class {
            req = req.with_latency_class(lc);
        }
        if let Some(tokens) = min_context {
            req = req.with_context_tokens(tokens);
        }
        if let Some(tokens) = max_output {
            req = req.with_max_output_tokens(tokens);
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

// Skill-keyed tool narrowing retired — see `crate::intent_policy`
// for the replacement. The narrow-by-skill function and its
// audit-gap warn tracker were Phase 1 of the Tool-Mastery framework;
// the retire-skills-menu plan moves the keying axis to intent +
// mode. Per-skill TOOL declarations on the surviving modes
// (inner-work, recipe-author) are now consumed by
// `intent_policy::policy_for` instead.

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
        // Note: the input TOML still carries a `[routing]` block —
        // Serde silently ignores it now that the field is retired
        // (see SkillToml docstring). The test exercises that
        // back-compat path.
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

    #[test]
    fn register_defaults_to_factual_when_absent() {
        let toml = r#"
[skill]
id = "test"
name = "Test"
version = "0.1.0"
"#;
        let skill = parse_skill_toml(toml).unwrap();
        assert_eq!(skill.inference.register, SkillRegister::Factual);
    }

    #[test]
    fn register_relational_round_trips() {
        let toml = r#"
[skill]
id = "test"
name = "Test"
version = "0.1.0"

[inference]
register = "relational"
"#;
        let skill = parse_skill_toml(toml).unwrap();
        assert_eq!(skill.inference.register, SkillRegister::Relational);
    }

    #[test]
    fn primary_skill_register_falls_back_to_factual_when_no_active_skill() {
        let reg = SkillRegistry::new();
        assert_eq!(reg.primary_skill_register(), SkillRegister::Factual);
    }

    #[test]
    fn primary_skill_register_resolves_active_relational_skill() {
        let mut reg = SkillRegistry::new();
        let toml = r#"
[skill]
id = "inner-test"
name = "Inner Test"
version = "0.1.0"

[inference]
privacy = "local_only"
register = "relational"
"#;
        reg.register(parse_skill_toml(toml).unwrap());
        reg.activate("inner-test");
        assert_eq!(reg.primary_skill_register(), SkillRegister::Relational);
    }

    /// `inner-work` is now the sole surviving relational mode.
    /// (`personal-assistant` was retired in the skills-menu cleanup.)
    /// Pin that the file parses and the register hasn't drifted —
    /// the relational voice contract at the ~14 register-keyed
    /// runtime sites depends on this declaration.
    #[test]
    fn inner_work_mode_parses_with_relational_register() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR should be set for tests");
        let modes_dir = std::path::Path::new(&manifest_dir)
            .join("..")
            .join("..")
            .join("modes");

        let path = modes_dir.join("inner-work").join("skill.toml");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let skill = parse_skill_toml(&content)
            .unwrap_or_else(|| panic!("parse {}", path.display()));
        assert_eq!(skill.id, "inner-work");
        assert_eq!(
            skill.inference.register,
            SkillRegister::Relational,
            "inner-work must declare register=\"relational\""
        );
    }

    // ─── Surviving-modes declarations ──────────────────────────

    /// After the skill-retirement work, only two TOMLs live under
    /// `sovereign/modes/`. This test pins their shape so a future
    /// edit doesn't accidentally widen inner-work's tool surface or
    /// rename recipe-author's required tools without updating the
    /// `intent_policy::policy_for` mode arms. Each assertion comes
    /// from the principled design, not from an audited count.
    #[test]
    fn surviving_modes_declare_expected_tool_shape() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR should be set for tests");
        let modes_dir = std::path::Path::new(&manifest_dir)
            .join("..")
            .join("..")
            .join("modes");

        let inner_work_toml = std::fs::read_to_string(modes_dir.join("inner-work/skill.toml"))
            .expect("read modes/inner-work/skill.toml");
        let inner_work = parse_skill_toml(&inner_work_toml)
            .expect("parse modes/inner-work/skill.toml");
        assert_eq!(inner_work.id, "inner-work");
        assert_eq!(inner_work.inference.register, SkillRegister::Relational);
        assert!(
            inner_work.tool_config.required.is_empty()
                && inner_work.tool_config.optional.is_empty(),
            "inner-work declares no tools by design — reflective work \
             is not tool-mediated"
        );

        let recipe_author_toml =
            std::fs::read_to_string(modes_dir.join("recipe-author/skill.toml"))
                .expect("read modes/recipe-author/skill.toml");
        let recipe_author = parse_skill_toml(&recipe_author_toml)
            .expect("parse modes/recipe-author/skill.toml");
        assert_eq!(recipe_author.id, "recipe-author");
        // Spot-check the must-have recipe tools (matches the
        // intent_policy::recipe_author_tools() table).
        let required: HashSet<&str> =
            recipe_author.tool_config.required.iter().map(String::as_str).collect();
        for needed in ["recipe_validate", "recipe_test", "decision_log"] {
            assert!(
                required.contains(needed),
                "recipe-author must require '{needed}' (intent_policy table depends on it)"
            );
        }
    }
}
