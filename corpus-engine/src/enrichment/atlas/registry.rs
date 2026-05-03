//! Atlas ingestion registry — string-id dispatch for strategies.
//!
//! Mirrors `enrichment::domain_registry::DomainRegistry` and
//! `enrichment::pipeline::registry::PipelineRegistry`
//! (ARCH_PRINCIPLES §4). Adding a strategy is a single `register`
//! call; dispatch is by string id.

use std::collections::HashMap;
use std::sync::Arc;

use super::ingestion::AtlasIngestion;

/// Registry mapping strategy id strings to factory functions that
/// produce `Arc<dyn AtlasIngestion>` instances.
pub struct AtlasIngestionRegistry {
    strategies: HashMap<String, fn() -> Arc<dyn AtlasIngestion>>,
}

impl AtlasIngestionRegistry {
    /// Empty registry. Prefer `builtin()` unless a test wants an
    /// isolated registry.
    pub fn new() -> Self {
        Self { strategies: HashMap::new() }
    }

    /// Registry pre-loaded with every built-in strategy. Today that
    /// is only `extraction_first`; the adapter wraps the existing
    /// 8-phase `literary_atlas` runner behind the `AtlasIngestion`
    /// trait surface.
    ///
    /// Note: the adapter itself lives in
    /// `pipeline::pipelines::literary_atlas::ExtractionFirstAdapter`
    /// and is registered here to keep the registry free of
    /// strategy-specific imports beyond the trait.
    pub fn builtin() -> Self {
        let mut r = Self::new();
        // `extraction_first` is registered by
        // `pipeline::pipelines::literary_atlas::register_into`
        // below, which is invoked once the adapter exists. For now
        // the registry is empty; tests construct strategies
        // manually via `register`.
        register_builtins(&mut r);
        r
    }

    pub fn register(&mut self, id: &str, factory: fn() -> Arc<dyn AtlasIngestion>) {
        self.strategies.insert(id.to_string(), factory);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn AtlasIngestion>> {
        self.strategies.get(id).map(|f| f())
    }

    pub fn strategy_ids(&self) -> Vec<&str> {
        self.strategies.keys().map(String::as_str).collect()
    }
}

impl Default for AtlasIngestionRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Hook point for strategies that want to register themselves into a
/// builtin registry. Called from `AtlasIngestionRegistry::builtin`.
/// Strategies expose their own `register_into(&mut registry)`
/// function and this file threads them together.
fn register_builtins(registry: &mut AtlasIngestionRegistry) {
    crate::enrichment::pipeline::pipelines::literary_atlas::register_extraction_first(registry);
    crate::enrichment::atlas::strategies::structure_first::register_structure_first(registry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_strategies() {
        let r = AtlasIngestionRegistry::new();
        assert!(r.strategy_ids().is_empty());
    }

    #[test]
    fn unknown_id_returns_none() {
        let r = AtlasIngestionRegistry::new();
        assert!(r.get("nonexistent").is_none());
    }

    #[test]
    fn builtin_registers_extraction_first() {
        let r = AtlasIngestionRegistry::builtin();
        let s = r
            .get("extraction_first")
            .expect("extraction_first should be registered by builtin");
        assert_eq!(s.id(), "extraction_first");
    }

    #[test]
    fn builtin_registers_structure_first() {
        let r = AtlasIngestionRegistry::builtin();
        let s = r
            .get("structure_first")
            .expect("structure_first should be registered by builtin");
        assert_eq!(s.id(), "structure_first");
    }
}
