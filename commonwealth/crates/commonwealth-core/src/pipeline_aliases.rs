//! Pipeline aliases — the M4 entry point for "the model alias is the
//! entire orchestrator."
//!
//! Where `ModelAliasTable` resolves `commonwealth/fast` to OICP
//! capability requirements that the scheduler uses to pick a concrete
//! model, `PipelineAliasTable` resolves `commonwealth/sovereign-coder`
//! to a full pipeline: a middleware stack + a concrete model id +
//! an embedded context-injection configuration.
//!
//! The two tables coexist. `chat_completions` tries pipelines first
//! (by exact name; no globbing — pipelines are proper nouns, not
//! capability patterns), then falls through to the existing
//! `ModelAliasTable` on miss. A concrete model named `sovereign-coder`
//! still wins over the pipeline alias because the exact-name match at
//! priority 2 of the handler runs before pipeline resolution.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Resolved pipeline: everything the middleware executor needs to
/// run a request against the concrete model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineResolution {
    pub name: String,
    pub model_id: String,
    pub middleware: Vec<String>,
    pub context: PipelineContextConfig,
}

/// Inline context-injection settings that travel with the pipeline
/// and are consumed by `ContextInjector`. Kept as data on the
/// pipeline so different aliases can share the same `ContextInjector`
/// impl while producing different preambles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineContextConfig {
    pub inject_notes: bool,
    pub inject_spec: bool,
    /// When true, the spec.md injection is limited to the
    /// `## Invariants` section only (red-team case).
    pub inject_invariants_only: bool,
}

impl Default for PipelineContextConfig {
    fn default() -> Self {
        Self {
            inject_notes: true,
            inject_spec: true,
            inject_invariants_only: false,
        }
    }
}

/// Lookup table over pipeline aliases. Exact-name match only — no
/// glob patterns. Pipelines are a small, curated set (order of ten,
/// not hundreds); globbing would invite accidental collisions.
#[derive(Debug, Clone, Default)]
pub struct PipelineAliasTable {
    by_name: HashMap<String, PipelineResolution>,
}

impl PipelineAliasTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, resolution: PipelineResolution) {
        self.by_name.insert(resolution.name.clone(), resolution);
    }

    /// Resolve a client-supplied model name. Accepts both the bare
    /// pipeline name (`"sovereign-coder"`) and the `"<provider>/<name>"`
    /// namespaced form opencode typically sends
    /// (`"commonwealth/sovereign-coder"`). The provider portion is
    /// stripped before lookup; only the trailing name matters.
    pub fn resolve(&self, model_name: &str) -> Option<&PipelineResolution> {
        let tail = model_name.rsplit('/').next().unwrap_or(model_name);
        self.by_name.get(tail)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// List the pipeline names in the table. Ordering is not
    /// guaranteed — kept as a discovery helper for operator tooling.
    pub fn names(&self) -> Vec<&str> {
        self.by_name.keys().map(|k| k.as_str()).collect()
    }

    /// Build the default table from the crate-embedded TOML.
    /// Panics on malformed TOML — the file is compiled in, so a bad
    /// entry is a build-time bug.
    pub fn default_table() -> Self {
        let text = include_str!("default_pipelines.toml");
        Self::from_toml(text).expect("default_pipelines.toml must parse")
    }

    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        let file: PipelineFile = toml::from_str(text)?;
        let mut table = Self::new();
        for entry in file.pipeline {
            table.insert(PipelineResolution {
                name: entry.name,
                model_id: entry.model_id,
                middleware: entry.middleware,
                context: entry.context.unwrap_or_default(),
            });
        }
        Ok(table)
    }
}

// ─── TOML shape ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PipelineFile {
    #[serde(default)]
    pipeline: Vec<PipelineEntry>,
}

#[derive(Debug, Deserialize)]
struct PipelineEntry {
    name: String,
    model_id: String,
    middleware: Vec<String>,
    context: Option<PipelineContextConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_table_loads() {
        let table = PipelineAliasTable::default_table();
        assert!(!table.is_empty());
        assert!(table.resolve("sovereign-coder").is_some());
        assert!(table.resolve("sovereign-red-team").is_some());
    }

    #[test]
    fn resolve_strips_provider_prefix() {
        let table = PipelineAliasTable::default_table();
        let direct = table.resolve("sovereign-coder").cloned();
        let namespaced = table.resolve("commonwealth/sovereign-coder").cloned();
        assert_eq!(direct, namespaced);
    }

    #[test]
    fn unknown_alias_returns_none() {
        let table = PipelineAliasTable::default_table();
        assert!(table.resolve("not-a-pipeline").is_none());
        assert!(table.resolve("commonwealth/phantom").is_none());
    }

    #[test]
    fn sovereign_coder_pipeline_shape() {
        let table = PipelineAliasTable::default_table();
        let p = table.resolve("sovereign-coder").unwrap();
        assert_eq!(p.model_id, "qwen-27b-coder");
        assert!(p.middleware.contains(&"approval_gate".to_string()));
        assert!(p.middleware.contains(&"session_briefing".to_string()));
        assert!(p.middleware.contains(&"context_injector".to_string()));
        assert!(p.middleware.contains(&"tool_injector".to_string()));
        assert!(p.middleware.contains(&"artifact_surface".to_string()));
        assert!(p.context.inject_notes);
        assert!(p.context.inject_spec);
        assert!(!p.context.inject_invariants_only);
    }

    #[test]
    fn red_team_pipeline_has_restricted_context() {
        let table = PipelineAliasTable::default_table();
        let p = table.resolve("sovereign-red-team").unwrap();
        assert!(!p.context.inject_notes);
        assert!(p.context.inject_spec);
        assert!(p.context.inject_invariants_only);
        assert!(p.middleware.contains(&"read_only_enforcer".to_string()));
    }

    #[test]
    fn from_toml_roundtrips_minimal() {
        let text = r#"
            [[pipeline]]
            name = "minimal"
            model_id = "some-model"
            middleware = []
        "#;
        let table = PipelineAliasTable::from_toml(text).unwrap();
        let p = table.resolve("minimal").unwrap();
        assert_eq!(p.model_id, "some-model");
        assert!(p.middleware.is_empty());
        // Context defaults kick in when omitted.
        assert!(p.context.inject_notes);
    }

    #[test]
    fn from_toml_empty_is_empty_table() {
        let table = PipelineAliasTable::from_toml("").unwrap();
        assert!(table.is_empty());
    }
}
