//! `AgentRunnerRegistry` — open-set dispatch for concrete runners.
//!
//! Mirrors `corpus-engine::enrichment::domain_registry::DomainRegistry`
//! per ARCH §4.2. New runners (opencode, codex, aider, …) slot in via
//! `register(...)`; the CLI looks up `--agent <id>` and never matches
//! on a string constant inline.

use std::collections::HashMap;
use std::sync::Arc;

use crate::runner::AgentRunner;
use crate::runners::{MockAgentRunner, PiRunner};

type RunnerFactory = Box<dyn Fn() -> Arc<dyn AgentRunner> + Send + Sync>;

pub struct AgentRunnerRegistry {
    factories: HashMap<&'static str, RunnerFactory>,
}

impl AgentRunnerRegistry {
    /// Empty registry. Useful in tests where only `MockAgentRunner`
    /// should be registered.
    pub fn empty() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Default registry — every shipped runner. The MVS ships `pi`
    /// and the `mock` runner (the latter exists for the test
    /// harness; registering it is cheap and lets `--agent mock` work
    /// from the CLI for smoke probes).
    pub fn builtin() -> Self {
        let mut r = Self::empty();
        r.register("pi", || Arc::new(PiRunner::new()));
        r.register("mock", || Arc::new(MockAgentRunner::canned()));
        r
    }

    pub fn register<F>(&mut self, id: &'static str, factory: F)
    where
        F: Fn() -> Arc<dyn AgentRunner> + Send + Sync + 'static,
    {
        self.factories.insert(id, Box::new(factory));
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn AgentRunner>> {
        self.factories.get(id).map(|f| f())
    }

    /// Sorted list of registered ids. Used by CLI error messages so a
    /// typo at `--agent` produces an actionable list.
    pub fn agent_ids(&self) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = self.factories.keys().copied().collect();
        ids.sort_unstable();
        ids
    }
}

impl Default for AgentRunnerRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registers_pi_and_mock() {
        let r = AgentRunnerRegistry::builtin();
        let ids = r.agent_ids();
        assert!(ids.contains(&"pi"), "expected `pi` in {ids:?}");
        assert!(ids.contains(&"mock"), "expected `mock` in {ids:?}");
        assert!(r.get("pi").is_some());
        assert!(r.get("mock").is_some());
    }

    #[test]
    fn unknown_id_returns_none() {
        let r = AgentRunnerRegistry::builtin();
        assert!(r.get("does-not-exist").is_none());
    }

    #[test]
    fn empty_starts_with_nothing() {
        let r = AgentRunnerRegistry::empty();
        assert!(r.agent_ids().is_empty());
    }

    #[test]
    fn register_adds_runner() {
        let mut r = AgentRunnerRegistry::empty();
        r.register("custom", || Arc::new(MockAgentRunner::canned()));
        assert!(r.get("custom").is_some());
    }
}
