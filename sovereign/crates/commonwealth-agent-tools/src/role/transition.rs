//! Pure-data transition rules. One function maps `(current_role,
//! triggering_primitive)` to the next role (or `None` =
//! terminate). Native runner calls this after each tool dispatch
//! to decide whether to flip roles before the next chat
//! completion request.

use crate::primitive::PrimitiveKind;
use crate::role::Role;

/// Why a transition fired — for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionTrigger {
    /// Primitive call triggered the transition (e.g. `write_file`
    /// implicitly hands off; `agent_plan` explicitly hands off).
    Primitive(PrimitiveKind),
    /// Agent emitted no tool call — implicit handoff per the
    /// transition rule (Planner → Implementer; Implementer →
    /// Evaluator; Evaluator stays).
    NoToolCall,
}

/// Compute next role after the active role's turn ended with the
/// given trigger. Returns `Some(next_role)` to flip, `None` to
/// terminate the run, or `Some(current)` to stay in the same role.
pub fn transition_after(
    current: Role,
    trigger: TransitionTrigger,
) -> NextRole {
    match (current, trigger) {
        // Termination: agent_done from any role.
        (_, TransitionTrigger::Primitive(PrimitiveKind::AgentDone)) => NextRole::Terminate,

        // Planner explicit handoff via agent_plan.
        (Role::Planner, TransitionTrigger::Primitive(PrimitiveKind::AgentPlan)) => {
            NextRole::Flip(Role::Implementer)
        }
        // Planner implicit (no tool call) → still hand off; Planner
        // shouldn't do nothing.
        (Role::Planner, TransitionTrigger::NoToolCall) => NextRole::Flip(Role::Implementer),
        // Planner inspecting workdir is "stay" — needs a follow-up
        // to emit agent_plan.
        (Role::Planner, TransitionTrigger::Primitive(PrimitiveKind::InspectWorkdir)) => {
            NextRole::Stay
        }

        // Implementer: write_file, patch_file, or explicit handoff
        // → Evaluator. patch_file is symmetric with write_file: both
        // mutate the workdir and both yield to the verifier.
        (Role::Implementer, TransitionTrigger::Primitive(PrimitiveKind::WriteFile)) => {
            NextRole::Flip(Role::Evaluator)
        }
        (Role::Implementer, TransitionTrigger::Primitive(PrimitiveKind::PatchFile)) => {
            NextRole::Flip(Role::Evaluator)
        }
        (Role::Implementer, TransitionTrigger::Primitive(PrimitiveKind::ReplaceFunction)) => {
            NextRole::Flip(Role::Evaluator)
        }
        (
            Role::Implementer,
            TransitionTrigger::Primitive(PrimitiveKind::HandoffToEvaluator),
        ) => NextRole::Flip(Role::Evaluator),
        // Inspect doesn't flip — Implementer can re-look without
        // committing to a write.
        (Role::Implementer, TransitionTrigger::Primitive(PrimitiveKind::InspectWorkdir)) => {
            NextRole::Stay
        }
        // No tool call: implicit handoff.
        (Role::Implementer, TransitionTrigger::NoToolCall) => NextRole::Flip(Role::Evaluator),

        // Evaluator: handoff_to_implementer flips back.
        (
            Role::Evaluator,
            TransitionTrigger::Primitive(PrimitiveKind::HandoffToImplementer),
        ) => NextRole::Flip(Role::Implementer),
        // build / smoke don't flip; Evaluator continues deciding.
        (Role::Evaluator, TransitionTrigger::Primitive(PrimitiveKind::Build)) => {
            NextRole::Stay
        }
        (Role::Evaluator, TransitionTrigger::Primitive(PrimitiveKind::Smoke)) => {
            NextRole::Stay
        }
        // No tool call: Evaluator's forced first tool already ran;
        // assume it's deciding — stay.
        (Role::Evaluator, TransitionTrigger::NoToolCall) => NextRole::Stay,

        // Anything else (wrong tool in wrong role, etc.) — keep
        // the run alive but stay in current role. The adapter's
        // `Unrecognized` outcome surfaces in telemetry; the
        // transition rule shouldn't terminate on tool-misuse.
        (_role, _) => NextRole::Stay,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextRole {
    Stay,
    Flip(Role),
    Terminate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_agent_plan_flips_to_implementer() {
        let next = transition_after(
            Role::Planner,
            TransitionTrigger::Primitive(PrimitiveKind::AgentPlan),
        );
        assert_eq!(next, NextRole::Flip(Role::Implementer));
    }

    #[test]
    fn planner_inspect_stays() {
        let next = transition_after(
            Role::Planner,
            TransitionTrigger::Primitive(PrimitiveKind::InspectWorkdir),
        );
        assert_eq!(next, NextRole::Stay);
    }

    #[test]
    fn implementer_write_file_flips_to_evaluator() {
        let next = transition_after(
            Role::Implementer,
            TransitionTrigger::Primitive(PrimitiveKind::WriteFile),
        );
        assert_eq!(next, NextRole::Flip(Role::Evaluator));
    }

    #[test]
    fn implementer_patch_file_flips_to_evaluator() {
        // patch_file must be symmetric with write_file at the
        // transition layer: both yield the workdir to the verifier.
        // If a future PR makes patch stay-in-Implementer, the
        // Implementer can patch repeatedly without verification —
        // re-opens the write-thrash class on a different primitive.
        let next = transition_after(
            Role::Implementer,
            TransitionTrigger::Primitive(PrimitiveKind::PatchFile),
        );
        assert_eq!(next, NextRole::Flip(Role::Evaluator));
    }

    #[test]
    fn implementer_handoff_flips_to_evaluator() {
        let next = transition_after(
            Role::Implementer,
            TransitionTrigger::Primitive(PrimitiveKind::HandoffToEvaluator),
        );
        assert_eq!(next, NextRole::Flip(Role::Evaluator));
    }

    #[test]
    fn evaluator_handoff_flips_to_implementer() {
        let next = transition_after(
            Role::Evaluator,
            TransitionTrigger::Primitive(PrimitiveKind::HandoffToImplementer),
        );
        assert_eq!(next, NextRole::Flip(Role::Implementer));
    }

    #[test]
    fn evaluator_build_stays() {
        let next = transition_after(
            Role::Evaluator,
            TransitionTrigger::Primitive(PrimitiveKind::Build),
        );
        assert_eq!(next, NextRole::Stay);
    }

    #[test]
    fn agent_done_from_any_role_terminates() {
        for r in Role::all() {
            let next = transition_after(
                *r,
                TransitionTrigger::Primitive(PrimitiveKind::AgentDone),
            );
            assert_eq!(next, NextRole::Terminate);
        }
    }

    #[test]
    fn no_tool_call_from_planner_flips_to_implementer() {
        let next = transition_after(Role::Planner, TransitionTrigger::NoToolCall);
        assert_eq!(next, NextRole::Flip(Role::Implementer));
    }

    #[test]
    fn no_tool_call_from_implementer_flips_to_evaluator() {
        let next = transition_after(Role::Implementer, TransitionTrigger::NoToolCall);
        assert_eq!(next, NextRole::Flip(Role::Evaluator));
    }
}
