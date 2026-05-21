//! `RoleProfile` — the data carrier for a role's invocation
//! shape.
//!
//! Profiles are *data*, per ARCH §6: system prompts + sampling
//! overrides + tool subsets live in TOML (or compiled-in defaults
//! for test stability). Operator tuning of the Evaluator's voice
//! doesn't require a code change.

use serde::{Deserialize, Serialize};

use crate::primitive::PrimitiveKind;
use crate::role::Role;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleProfile {
    pub role: Role,
    pub system_prompt: String,
    pub allowed_primitives: Vec<PrimitiveKind>,
    pub sampling: SamplingOverrides,
    /// If `Some`, the FIRST tool call this role emits is forced to
    /// the named primitive (via OpenAI `tool_choice`). Subsequent
    /// turns within the same role are free choice over
    /// `allowed_primitives`.
    pub forced_first_tool: Option<PrimitiveKind>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SamplingOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// Compiled-in default profile for a role. Used when no TOML
/// override is present and as the test-stability anchor.
pub fn default_profile_for(role: Role) -> RoleProfile {
    match role {
        Role::Planner => RoleProfile {
            role: Role::Planner,
            system_prompt:
                "You are the Planner. Your job is exactly one tool call: `agent_plan` with a \
                 3-6 sentence plan covering (a) data structures, (b) algorithm in one phrase, \
                 (c) files to write or modify. The workdir contents are already listed in your \
                 user message — you have everything you need. Do not inspect, do not write \
                 code, do not build. The Implementer reads your plan and executes it."
                    .to_string(),
            allowed_primitives: vec![PrimitiveKind::AgentPlan],
            sampling: SamplingOverrides {
                temperature: Some(0.4),
                ..Default::default()
            },
            forced_first_tool: Some(PrimitiveKind::AgentPlan),
        },
        Role::Implementer => RoleProfile {
            role: Role::Implementer,
            system_prompt:
                "You are the Implementer. The Planner's plan and the workdir state are in \
                 your context. Use `write_file` to make exactly one change. After writing, \
                 call `handoff_to_evaluator` with a one-line `what_you_changed` summary. If \
                 the last Evaluator diagnosis says the tests passed, call `agent_done`. You \
                 do not have an `inspect_workdir` tool here — the workdir contents are \
                 already listed in the user message."
                    .to_string(),
            allowed_primitives: vec![
                PrimitiveKind::WriteFile,
                PrimitiveKind::HandoffToEvaluator,
                PrimitiveKind::AgentDone,
            ],
            sampling: SamplingOverrides::default(),
            forced_first_tool: Some(PrimitiveKind::WriteFile),
        },
        Role::Evaluator => RoleProfile {
            role: Role::Evaluator,
            system_prompt:
                "You are the Evaluator. The Implementer just made a change. Your job is a \
                 short sequence: call `build`, then if build succeeds call `smoke`, then call \
                 EITHER `agent_done` (if smoke passed all tests) OR `handoff_to_implementer` \
                 with a one-paragraph diagnosis (if build failed, smoke failed, or any test \
                 failed). NEVER call the same tool twice in a row — if you just built, your \
                 next call is smoke, handoff, or done."
                    .to_string(),
            allowed_primitives: vec![
                PrimitiveKind::Build,
                PrimitiveKind::Smoke,
                PrimitiveKind::HandoffToImplementer,
                PrimitiveKind::AgentDone,
            ],
            sampling: SamplingOverrides {
                temperature: Some(0.5),
                ..Default::default()
            },
            forced_first_tool: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_subset_excludes_write_and_build() {
        let p = default_profile_for(Role::Planner);
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::WriteFile));
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::Build));
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::Smoke));
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::InspectWorkdir));
        assert!(p.allowed_primitives.contains(&PrimitiveKind::AgentPlan));
    }

    #[test]
    fn implementer_subset_excludes_build_inspect_and_plan() {
        let p = default_profile_for(Role::Implementer);
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::Build));
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::Smoke));
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::AgentPlan));
        // Inspect excluded: workdir state is in the initial user
        // message; Implementer that "needs to inspect" defaults to
        // looping inspect instead of writing. Structural fix.
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::InspectWorkdir));
        assert!(p.allowed_primitives.contains(&PrimitiveKind::WriteFile));
    }

    #[test]
    fn evaluator_subset_excludes_write_and_plan() {
        let p = default_profile_for(Role::Evaluator);
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::WriteFile));
        assert!(!p.allowed_primitives.contains(&PrimitiveKind::AgentPlan));
        assert!(p.allowed_primitives.contains(&PrimitiveKind::Build));
    }

    #[test]
    fn planner_forces_agent_plan_first() {
        // 2026-05-21 measurement: 35B primary in Planner role with
        // free choice over {inspect, agent_plan} kept calling
        // inspect on Cargo.toml and never emitted agent_plan.
        // Forcing agent_plan structurally closes the inspect-loop.
        let p = default_profile_for(Role::Planner);
        assert_eq!(p.forced_first_tool, Some(PrimitiveKind::AgentPlan));
    }

    #[test]
    fn evaluator_no_forced_first_tool() {
        // After 2026-05-21 measurement: forced build on the first
        // Evaluator turn caused the model to keep calling build on
        // subsequent turns ("the harness said build first"). Free
        // choice + the same-primitive loop detector measures more
        // honestly.
        let p = default_profile_for(Role::Evaluator);
        assert_eq!(p.forced_first_tool, None);
    }

    #[test]
    fn implementer_forces_write_first() {
        // Per the 2026-05-21 measurement: 35B primary under Implementer
        // role with `tool_choice = auto` defaulted to `inspect_workdir`
        // every turn, never writing. Forcing write_file on the first
        // Implementer turn closes the inspect-loop class structurally.
        let p = default_profile_for(Role::Implementer);
        assert_eq!(p.forced_first_tool, Some(PrimitiveKind::WriteFile));
    }
}
