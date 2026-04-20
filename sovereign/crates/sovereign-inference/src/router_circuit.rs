//! `RouterCircuitChecker` — monitors the LLM router circuit breaker.
//!
//! When the primary backend accumulates ≥ 3 consecutive failures, `HybridProvider`
//! marks it unavailable (circuit open).  This checker detects that state and
//! attempts to close it by sending a lightweight probe.

use std::sync::Arc;

use sovereign_core::error::{Error, Result};
use sovereign_core::health::{
    Component, HealthCheckable, HealthIssue, HealthReport, RepairOutcome,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

use crate::health::HealthTracker;

// ─── RouterCircuitChecker ────────────────────────────────────────────────────

pub struct RouterCircuitChecker {
    tracker: Arc<HealthTracker>,
    inference: Arc<dyn InferenceProvider>,
}

impl RouterCircuitChecker {
    pub fn new(
        tracker: Arc<HealthTracker>,
        inference: Arc<dyn InferenceProvider>,
    ) -> Self {
        Self { tracker, inference }
    }
}

impl HealthCheckable for RouterCircuitChecker {
    fn component(&self) -> Component {
        Component::LlmRouter
    }

    fn check(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HealthReport>> + Send + '_>>
    {
        Box::pin(async move {
            let mut issues = Vec::new();

            if !self.tracker.is_healthy() {
                let failure_count = self.tracker.error_count();
                issues.push(HealthIssue::RouterCircuitOpen {
                    failure_count,
                    last_error: format!("{failure_count} consecutive failures"),
                    fallback_active: true,
                });
            }

            Ok(HealthReport::from_issues(Component::LlmRouter, issues))
        })
    }

    fn repair(
        &self,
        issue: &HealthIssue,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RepairOutcome>> + Send + '_>,
    > {
        let issue = issue.clone();
        Box::pin(async move {
            match &issue {
                HealthIssue::RouterCircuitOpen { .. } => {
                    // Send a minimal probe to see if the backend has recovered.
                    let probe = CompletionRequest {
                        prompt: "ping".into(),
                        system_message: None,
                        preferred_speed: Speed::Fast,
                        max_tokens: Some(1),
                        temperature: Some(0.0),
                        structured_output: None,
                        think_budget: None,
                        top_k: None,
                        top_p: None,
                        oicp: None,
            tools: None,
            tool_choice: None,
                    };
                    match self.inference.complete(&probe).await {
                        Ok(_) => {
                            self.tracker.reset_errors();
                            Ok(RepairOutcome::Resolved)
                        }
                        Err(e) => Ok(RepairOutcome::Failed {
                            reason: format!("Probe failed: {e}"),
                        }),
                    }
                }
                _ => Err(Error::RepairNotSupported),
            }
        })
    }

    fn can_repair_autonomously(&self, issue: &HealthIssue) -> bool {
        // Circuit probe is cheap and safe to run automatically.
        matches!(issue, HealthIssue::RouterCircuitOpen { .. })
    }
}
