//! Role layer — the counter-force structure on top of the
//! canonical primitives.
//!
//! A `Role` is a closed enum of agent invocation profiles. Each
//! role carries a `RoleProfile`: system prompt + allowed primitive
//! subset + sampling overrides + forced first tool. The agent loop
//! runs in one role at a time; transitions between roles happen at
//! call boundaries (each chat completion request uses the active
//! role's profile).
//!
//! Three roles in v1:
//!
//! - **Planner** — read state + emit chunked plan. No write/build
//!   access. Closes "Implementer holding the whole problem in
//!   attention."
//! - **Implementer** — write code against the plan. No build/smoke
//!   access. After every `write_file` (or `handoff_to_evaluator`),
//!   transition to Evaluator. Closes "Implementer iterates without
//!   verifying."
//! - **Evaluator** — verify state + decide next move. First call
//!   forced to `build` or `smoke`. No `write_file` access. Closes
//!   verify-discipline gap structurally.
//!
//! Plans live in `~/.claude/plans/role-layer-multilang.md`.

pub mod dossier;
pub mod model_map;
pub mod profile;
pub mod transition;

use serde::{Deserialize, Serialize};

pub use dossier::{RoleDossier, RoleDossierOutcome};
pub use model_map::RoleModelMap;
pub use profile::{
    RoleProfile, SamplingOverrides, EVALUATOR_MUST_HANDOFF_SUBSET, EVALUATOR_TERMINATING_SUBSET,
};
pub use transition::{transition_after, TransitionTrigger};

/// Closed enum of agent roles. Adding a variant requires touching:
/// `profile::default_profile_for`, `transition::transition_after`,
/// `transition::initial_role`, and every site that matches on
/// `Role` exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Planner,
    Implementer,
    Evaluator,
}

impl Role {
    pub const fn id(&self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Implementer => "implementer",
            Role::Evaluator => "evaluator",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "planner" => Some(Role::Planner),
            "implementer" => Some(Role::Implementer),
            "evaluator" => Some(Role::Evaluator),
            _ => None,
        }
    }

    pub const fn all() -> &'static [Role] {
        &[Role::Planner, Role::Implementer, Role::Evaluator]
    }

    /// The role the agent loop starts in. Per the plan: Planner
    /// runs first so the Implementer inherits a chunked plan
    /// instead of holding the whole problem.
    pub const fn initial() -> Role {
        Role::Planner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_ids_round_trip() {
        for r in Role::all() {
            assert_eq!(Role::from_id(r.id()), Some(*r));
        }
    }

    #[test]
    fn initial_is_planner() {
        assert_eq!(Role::initial(), Role::Planner);
    }
}
