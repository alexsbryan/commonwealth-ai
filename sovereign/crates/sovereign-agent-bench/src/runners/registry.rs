//! `AgentRunnerRegistry` — open-set dispatch for concrete runners.
//!
//! Mirrors `corpus-engine::enrichment::domain_registry::DomainRegistry`
//! per ARCH §4.2. New runners (opencode, codex, aider, …) slot in via
//! `register(...)`; the CLI looks up `--agent <id>` and never matches
//! on a string constant inline.

use std::collections::HashMap;
use std::sync::Arc;

use crate::runner::AgentRunner;
use crate::runners::{BareMetalRunner, MockAgentRunner, NativeRunner, PiRunner, SearchRunner};

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
        r.register("native", || Arc::new(NativeRunner::new()));
        // PR-1 baseline preserved for the role-layer A/B/C comparison.
        // Same daemon, same canonical primitives, no role transitions
        // — measures the verify-discipline gap without the role
        // layer's structural counter-force.
        r.register("native-monolithic", || Arc::new(NativeRunner::monolithic()));
        // 2026-05-24: validated as bench's strongest agent shape —
        // parallel candidate generation with monotonic-improvement
        // gating, no role split, no defensive parsing. See search.rs
        // module doc + the wildcard-rebuild session notes.
        r.register("search", || Arc::new(SearchRunner::new()));
        // Minimum-ceremony baseline; ships alongside search so
        // operators can A/B "is orchestration earning its keep here".
        r.register("bare-metal", || Arc::new(BareMetalRunner::new()));
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
    fn builtin_registers_pi_native_and_mock() {
        let r = AgentRunnerRegistry::builtin();
        let ids = r.agent_ids();
        assert!(ids.contains(&"pi"), "expected `pi` in {ids:?}");
        assert!(ids.contains(&"native"), "expected `native` in {ids:?}");
        assert!(
            ids.contains(&"native-monolithic"),
            "expected `native-monolithic` in {ids:?}"
        );
        assert!(ids.contains(&"mock"), "expected `mock` in {ids:?}");
        assert!(r.get("pi").is_some());
        assert!(r.get("native").is_some());
        assert!(r.get("native-monolithic").is_some());
        assert!(r.get("mock").is_some());
    }

    #[test]
    fn builtin_registers_search_and_bare_metal() {
        // The 2026-05-24 wildcard rebuild added these as
        // first-class options. `search` is the validated new
        // primary; `bare-metal` is the baseline. Future PRs may
        // flip the bench's default `--agent` to search; that
        // change should be guarded by a separate test.
        let r = AgentRunnerRegistry::builtin();
        let ids = r.agent_ids();
        assert!(ids.contains(&"search"), "expected `search` in {ids:?}");
        assert!(
            ids.contains(&"bare-metal"),
            "expected `bare-metal` in {ids:?}"
        );
        assert!(r.get("search").is_some());
        assert!(r.get("bare-metal").is_some());
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
