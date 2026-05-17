use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::traits::{Planner, Router};
use crate::types::*;

/// Router that always returns SimpleQuery with maximum confidence.
/// Used for tests and as the Phase-2 default when no LLM router is
/// wired. Confidence of 1.0 guarantees `decide_policy` picks
/// `MoveKind::Commit` — the stub must never inadvertently trigger a
/// clarification card in a test harness.
pub struct PassthroughRouter;

#[async_trait]
impl Router for PassthroughRouter {
    async fn classify(
        &self,
        _message: &str,
        _context: &ConversationContext,
        _available_tools: &[ToolDescriptor],
    ) -> Result<RouterClassification> {
        Ok(RouterClassification {
            primary: IntentCandidate {
                intent: Intent::SimpleQuery,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: None,
            coarse_intent: None,
            self_assessment: None,
            timing: None,
            scope: None,
        })
    }
}

/// Planner that always returns an error. Used until Phase 5.
pub struct NoOpPlanner;

#[async_trait]
impl Planner for NoOpPlanner {
    async fn plan(
        &self,
        _goal: &str,
        _context: &ConversationContext,
        _available_tools: &[ToolDescriptor],
    ) -> Result<Plan> {
        Err(Error::NotImplemented(
            "Planning not available yet".to_string(),
        ))
    }

    async fn replan(
        &self,
        _original: &Plan,
        _completed: &[(usize, StepOutput)],
        _failure: &StepError,
    ) -> Result<Plan> {
        Err(Error::NotImplemented(
            "Replanning not available yet".to_string(),
        ))
    }
}
