use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::traits::{Planner, Router};
use crate::types::*;

/// Router that always returns SimpleQuery. Used for testing and Phase 2.
pub struct PassthroughRouter;

#[async_trait]
impl Router for PassthroughRouter {
    async fn classify(
        &self,
        _message: &str,
        _context: &ConversationContext,
        _available_tools: &[ToolDescriptor],
    ) -> Result<RoutingOutcome> {
        Ok(RoutingOutcome {
            intent: Intent::SimpleQuery,
            coarse_intent: None,
            self_assessment: None,
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
