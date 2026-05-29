//! Domain registry — maps domain ID strings to factory functions.
//!
//! Replaces the match statement in `FieldModelEngine::from_recipe()` so
//! new domains can be registered without modifying the engine.

use std::collections::HashMap;
use std::sync::Arc;

use super::domain::Domain;

/// Registry mapping domain ID strings to factory functions that produce
/// `Arc<dyn Domain>` instances.
pub struct DomainRegistry {
    domains: HashMap<String, fn() -> Arc<dyn Domain>>,
}

impl DomainRegistry {
    /// Create a registry pre-loaded with all built-in domains.
    pub fn builtin() -> Self {
        let mut registry = Self {
            domains: HashMap::new(),
        };
        registry.register("philosophy", || {
            Arc::new(super::domains::philosophy::PhilosophyDomain)
        });
        registry.register("science", || {
            Arc::new(super::domains::science::ScienceDomain)
        });
        registry.register("policy", || {
            Arc::new(super::domains::policy::PolicyDomain)
        });
        registry.register("legal", || {
            Arc::new(super::domains::legal::LegalDomain)
        });
        registry.register("community", || {
            Arc::new(super::domains::community::CommunityKnowledgeDomain)
        });
        registry.register("multi", || {
            Arc::new(super::domains::multi::MultiDomain::wikipedia_default())
        });
        // Stub for the Stack Exchange knowledge layer. Registered so
        // recipes referencing it parse cleanly; prompt set is a
        // follow-on. See `domains::engineering` for the planned
        // epistemic vocabulary (Approach / Tradeoff / Context /
        // Assumption).
        registry.register("engineering", || {
            Arc::new(super::domains::engineering::EngineeringDomain)
        });
        // KnowledgeView domains — enrich SQLite-sourced corpora
        // (memories → personal, conversations → conversational).
        registry.register("personal", || {
            Arc::new(super::domains::personal::PersonalDomain)
        });
        registry.register("conversational", || {
            Arc::new(super::domains::conversational::ConversationalDomain)
        });
        registry.register("business_email", || {
            Arc::new(super::domains::business_email::BusinessEmailDomain::new())
        });
        registry.register("institutional", || {
            Arc::new(super::domains::institutional::InstitutionalDomain)
        });
        registry
    }

    /// Register a new domain factory.
    pub fn register(&mut self, id: &str, factory: fn() -> Arc<dyn Domain>) {
        self.domains.insert(id.to_string(), factory);
    }

    /// Look up a domain by ID. Returns `None` if unregistered.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Domain>> {
        self.domains.get(id).map(|f| f())
    }

    /// List all registered domain IDs.
    pub fn domain_ids(&self) -> Vec<&str> {
        self.domains.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_all_domains() {
        let reg = DomainRegistry::builtin();
        for id in [
            "philosophy",
            "science",
            "policy",
            "legal",
            "community",
            "multi",
            "engineering",
            "personal",
            "conversational",
            "business_email",
            "institutional",
        ] {
            assert!(reg.get(id).is_some(), "missing domain: {id}");
        }
    }

    #[test]
    fn unknown_domain_returns_none() {
        let reg = DomainRegistry::builtin();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn register_custom_domain() {
        let mut reg = DomainRegistry::builtin();
        reg.register("custom", || {
            Arc::new(super::super::domains::philosophy::PhilosophyDomain)
        });
        assert!(reg.get("custom").is_some());
    }
}
