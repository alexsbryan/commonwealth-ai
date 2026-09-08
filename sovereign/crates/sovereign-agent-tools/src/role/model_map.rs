// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RoleModelMap` — per-role daemon model handle.
//!
//! Lets the bench (or any role-aware caller) route different roles to
//! different slots: 35B primary for Implementer's algorithm work, 9B
//! fast for Planner's plan-only turn and Evaluator's
//! verify-then-decide turn. Drops daemon RSS peak on single-machine
//! benches (the 64 GB Mac jetsam class from HANDOFF §E) AND matches
//! production economics — burning the biggest model on every role is
//! wasteful in real coding sessions too.
//!
//! Pure data. `None` for every role degrades to the caller's fallback
//! model handle — single-flag operation is byte-identical to PR-2.
//!
//! Wired through `AgentRunContext.role_model_map` and consulted by
//! the native role-aware runner before each `build_role_request_body`
//! call. Captured into the run artifact (`role_model_map_used`) so
//! replay can reproduce.

use serde::{Deserialize, Serialize};

use crate::role::Role;

/// Optional per-role model overrides. Constructed once at run start
/// from CLI flags; queried once per role's request issuance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleModelMap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<String>,
}

impl RoleModelMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the per-role override. `None` clears any prior override
    /// for that role (the caller's fallback will be used at lookup).
    pub fn set(&mut self, role: Role, model: Option<String>) {
        match role {
            Role::Planner => self.planner = model,
            Role::Implementer => self.implementer = model,
            Role::Evaluator => self.evaluator = model,
        }
    }

    /// Override for `role`, if any was set.
    pub fn get(&self, role: Role) -> Option<&str> {
        match role {
            Role::Planner => self.planner.as_deref(),
            Role::Implementer => self.implementer.as_deref(),
            Role::Evaluator => self.evaluator.as_deref(),
        }
    }

    /// Resolve the model for `role`: per-role override if set, else
    /// `fallback`. The single call site in the role-aware runner.
    pub fn model_for<'a>(&'a self, role: Role, fallback: &'a str) -> &'a str {
        self.get(role).unwrap_or(fallback)
    }

    /// True when no role has an override — equivalent to the
    /// single-model PR-2 behavior.
    pub fn is_empty(&self) -> bool {
        self.planner.is_none() && self.implementer.is_none() && self.evaluator.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let m = RoleModelMap::new();
        assert!(m.is_empty());
        for r in Role::all() {
            assert!(m.get(*r).is_none());
        }
    }

    #[test]
    fn set_and_get_round_trip() {
        let mut m = RoleModelMap::new();
        m.set(Role::Planner, Some("commonwealth/coder".into()));
        m.set(Role::Implementer, Some("commonwealth/primary".into()));
        m.set(Role::Evaluator, Some("commonwealth/coder".into()));
        assert_eq!(m.get(Role::Planner), Some("commonwealth/coder"));
        assert_eq!(m.get(Role::Implementer), Some("commonwealth/primary"));
        assert_eq!(m.get(Role::Evaluator), Some("commonwealth/coder"));
        assert!(!m.is_empty());
    }

    #[test]
    fn model_for_returns_override_when_set() {
        let mut m = RoleModelMap::new();
        m.set(Role::Implementer, Some("commonwealth/primary".into()));
        assert_eq!(
            m.model_for(Role::Implementer, "commonwealth/fallback"),
            "commonwealth/primary"
        );
    }

    #[test]
    fn model_for_returns_fallback_when_unset() {
        let m = RoleModelMap::new();
        assert_eq!(
            m.model_for(Role::Planner, "commonwealth/fallback"),
            "commonwealth/fallback"
        );
    }

    #[test]
    fn model_for_partial_overrides_mix_with_fallback() {
        let mut m = RoleModelMap::new();
        m.set(Role::Planner, Some("commonwealth/coder".into()));
        // Implementer + Evaluator unset → fallback.
        assert_eq!(m.model_for(Role::Planner, "fallback"), "commonwealth/coder");
        assert_eq!(m.model_for(Role::Implementer, "fallback"), "fallback");
        assert_eq!(m.model_for(Role::Evaluator, "fallback"), "fallback");
    }

    #[test]
    fn set_none_clears_override() {
        let mut m = RoleModelMap::new();
        m.set(Role::Planner, Some("commonwealth/coder".into()));
        assert!(!m.is_empty());
        m.set(Role::Planner, None);
        assert!(m.is_empty());
        assert_eq!(m.model_for(Role::Planner, "fallback"), "fallback");
    }

    #[test]
    fn serde_round_trip_skips_none() {
        let mut m = RoleModelMap::new();
        m.set(Role::Implementer, Some("commonwealth/primary".into()));
        let json = serde_json::to_string(&m).unwrap();
        // None fields are skipped — only `implementer` lands in JSON.
        assert!(json.contains("implementer"));
        assert!(!json.contains("planner"));
        assert!(!json.contains("evaluator"));
        let back: RoleModelMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn serde_empty_round_trips_to_default() {
        let m = RoleModelMap::new();
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "{}");
        let back: RoleModelMap = serde_json::from_str(&json).unwrap();
        assert!(back.is_empty());
    }
}
