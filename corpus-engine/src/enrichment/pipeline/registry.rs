//! Pipeline registry — maps pipeline ID strings to factory functions.
//!
//! Mirrors `enrichment::domain_registry::DomainRegistry` (ARCH_PRINCIPLES §4).
//! Adding a pipeline is a single `register` call; dispatch is by
//! string id, not by match arm.

use std::collections::HashMap;
use std::sync::Arc;

use super::trait_def::Pipeline;

/// Registry mapping pipeline id strings to factory functions that
/// produce `Arc<dyn Pipeline>` instances.
pub struct PipelineRegistry {
    pipelines: HashMap<String, fn() -> Arc<dyn Pipeline>>,
}

impl PipelineRegistry {
    /// Empty registry. Prefer `builtin()` unless a test genuinely
    /// wants an isolated registry.
    pub fn new() -> Self {
        Self {
            pipelines: HashMap::new(),
        }
    }

    /// Registry pre-loaded with every built-in pipeline.
    pub fn builtin() -> Self {
        let mut r = Self::new();
        r.register("literary", || {
            Arc::new(super::pipelines::literary::LiteraryPipeline::new())
        });
        r.register(super::pipelines::literary_atlas::PIPELINE_ID, || {
            Arc::new(super::pipelines::literary_atlas::LiteraryAtlasPipeline::new())
        });
        r.register(super::pipelines::philosophy_atlas::PIPELINE_ID, || {
            Arc::new(super::pipelines::philosophy_atlas::PhilosophyAtlasPipeline::new())
        });
        r.register(super::pipelines::referential_atlas::PIPELINE_ID, || {
            Arc::new(super::pipelines::referential_atlas::ReferentialAtlasPipeline::new())
        });
        r.register(super::pipelines::engineering_atlas::PIPELINE_ID, || {
            Arc::new(super::pipelines::engineering_atlas::EngineeringAtlasPipeline::new())
        });
        // `obsidian_atlas` removed when the vault port replaced the
        // legacy Phase-1+ pipeline with the tiered RAPTOR + GLiNER
        // surface. Vault corpora now route through
        // `FolderTieredProvider` instead of this registry. Operators
        // who still want bench-scorable atoms.json output for a vault
        // can pass `--pipeline literary_atlas` explicitly.
        r.register(super::pipelines::conversation_atlas::PIPELINE_ID, || {
            Arc::new(super::pipelines::conversation_atlas::ConversationAtlasPipeline::new())
        });
        r
    }

    pub fn register(&mut self, id: &str, factory: fn() -> Arc<dyn Pipeline>) {
        self.pipelines.insert(id.to_string(), factory);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Pipeline>> {
        self.pipelines.get(id).map(|f| f())
    }

    pub fn pipeline_ids(&self) -> Vec<&str> {
        self.pipelines.keys().map(String::as_str).collect()
    }
}

impl Default for PipelineRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registers_literary() {
        let r = PipelineRegistry::builtin();
        assert!(r.get("literary").is_some());
    }

    #[test]
    fn builtin_registers_literary_atlas() {
        let r = PipelineRegistry::builtin();
        let p = r.get("literary_atlas").expect("literary_atlas registered");
        assert_eq!(p.id(), "literary_atlas");
    }

    #[test]
    fn builtin_registers_philosophy_atlas() {
        let r = PipelineRegistry::builtin();
        let p = r
            .get("philosophy_atlas")
            .expect("philosophy_atlas should be registered as a builtin pipeline");
        assert_eq!(p.id(), "philosophy_atlas");
        // Phase C Step 7's acceptance: the philosophy pipeline
        // opts into the configuration phase without any code
        // branches in the runner.
        assert!(p.runs_configuration_phase());
    }

    #[test]
    fn builtin_registers_referential_atlas() {
        let r = PipelineRegistry::builtin();
        let p = r
            .get("referential_atlas")
            .expect("referential_atlas should be registered as a builtin pipeline");
        assert_eq!(p.id(), "referential_atlas");
        // Referential corpora skip Phase 8 — there's no interpretive
        // rollup to produce when the source text is editorial neutral.
        assert!(!p.runs_configuration_phase());
    }

    #[test]
    fn obsidian_atlas_no_longer_registered() {
        // The vault port removed the legacy obsidian_atlas forwarding
        // wrapper in favour of the tiered surface. Operators wanting
        // atoms.json against a vault should pass --pipeline literary_atlas
        // explicitly.
        let r = PipelineRegistry::builtin();
        assert!(
            r.get("obsidian_atlas").is_none(),
            "obsidian_atlas registry entry should be removed; \
             vault corpora now flow through FolderTieredProvider"
        );
    }

    #[test]
    fn unknown_id_returns_none() {
        let r = PipelineRegistry::builtin();
        assert!(r.get("nonexistent").is_none());
    }

    #[test]
    fn register_custom_pipeline_round_trips() {
        let mut r = PipelineRegistry::new();
        r.register("literary", || {
            Arc::new(super::super::pipelines::literary::LiteraryPipeline::new())
        });
        assert_eq!(r.pipeline_ids(), vec!["literary"]);
        assert!(r.get("literary").is_some());
    }
}
