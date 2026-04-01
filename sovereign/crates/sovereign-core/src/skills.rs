use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::types::Intent;

// ─── Skill Definition ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub version: String,
    pub routing: RoutingHints,
    pub planner_templates: Vec<PlanTemplate>,
    pub tool_config: ToolPreferences,
    pub prompts: PromptOverrides,
    pub memory_rules: MemoryConfig,
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
    pub tool_settings: std::collections::HashMap<String, serde_json::Value>,
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

    pub fn activate(&mut self, skill_id: &str) {
        self.active.insert(skill_id.to_string());
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
