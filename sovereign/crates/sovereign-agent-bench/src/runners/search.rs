// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search-not-agent runner — thin adapter over
//! [`commonwealth_tdd::run_trial`].
//!
//! The actual solver loop lives in `commonwealth-tdd::trial` (the
//! collapsed surface as of 2026-05-24). This module just maps the
//! bench's `AgentRunContext` → `commonwealth_tdd::Trial` with
//! `Polarity::MaximizePassing` (the Green-equivalent default),
//! dispatches, and maps `TrialResult` back to `AgentRunArtifact`.
//! All loop semantics — parallel candidates, monotonic gating,
//! stall detection — are validated by `commonwealth-tdd`'s own
//! test suite.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::json;
use tracing::warn;

use commonwealth_tdd::{
    run_trial, ChatBackend, Polarity, ReqwestChatBackend, Trial, TrialConfig, TrialStatus, Workdir,
};

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ChatRequestRecord, ExitReason,
    TokenCounts,
};

pub struct SearchRunner {
    backend: Arc<dyn ChatBackend>,
}

impl SearchRunner {
    pub fn new() -> Self {
        Self::with_provider_url("http://localhost:9741/v1".into())
    }

    pub fn with_provider_url(provider_url: String) -> Self {
        Self {
            backend: Arc::new(ReqwestChatBackend::new(provider_url)),
        }
    }

    /// Lets tests inject a `DeterministicChatBackend` or other
    /// mock without going through HTTP.
    pub fn with_backend(backend: Arc<dyn ChatBackend>) -> Self {
        Self { backend }
    }
}

impl Default for SearchRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialize a fresh git repo + commit the scaffold so the
/// commonwealth-tdd Workdir gate accepts the bench's scratch dir.
/// Idempotent — if the dir is already a git repo we just return.
fn git_init_scaffold(path: &std::path::Path) -> std::io::Result<()> {
    use std::process::Command;
    if Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(());
    }
    let run = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .map(|_| ())
    };
    run(&["init", "--initial-branch=main"])?;
    run(&["config", "user.email", "bench@local"])?;
    run(&["config", "user.name", "bench"])?;
    run(&["add", "."])?;
    run(&["commit", "--allow-empty", "-m", "bench scaffold"])?;
    Ok(())
}

#[async_trait]
impl AgentRunner for SearchRunner {
    fn id(&self) -> &'static str {
        "search"
    }

    fn default_model_handle(&self) -> Option<&str> {
        Some("commonwealth/primary")
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        let started = Instant::now();

        if let Err(e) = git_init_scaffold(ctx.workdir.path()) {
            warn!(error = %e, "search: git init failed; proceeding with force=true");
        }
        let workdir = match Workdir::check_safe(ctx.workdir.path().to_path_buf(), true) {
            Ok(w) => w,
            Err(e) => {
                return Ok(crashed_artifact(
                    ctx.workdir,
                    started,
                    format!("workdir gate: {e}"),
                ));
            }
        };

        let trial = Trial {
            workdir,
            model: ctx.model_handle.clone(),
            prompt: ctx.prompt.clone(),
            test_command: ctx.verify_cmd.clone(),
            polarity: Polarity::MaximizePassing,
            config: TrialConfig::default(),
            // Bench passes the language-appropriate validator; the
            // executor rejects malformed code at apply time with
            // shaped feedback instead of writing it and failing
            // opaquely at test collection. Targets the trial-2-
            // style "model wrote unparseable Python that pytest
            // couldn't import" failure mode.
            syntax_validator: ctx.syntax_validator.clone(),
        };

        let result = run_trial(trial, Arc::clone(&self.backend)).await;

        // Map TrialStatus → ExitReason. The bench's downstream
        // judges expect SearchStalled / SearchExhaustedRounds /
        // Completed / Crashed; preserve those mappings exactly.
        let exit_reason = match result.status {
            TrialStatus::Reached | TrialStatus::Improved => ExitReason::Completed,
            TrialStatus::Stalled {
                rounds_without_improvement,
            } => ExitReason::SearchStalled {
                rounds_without_improvement,
            },
            TrialStatus::Exhausted { rounds } => ExitReason::SearchExhaustedRounds { rounds },
            TrialStatus::NoBaseline { reason } | TrialStatus::Errored { reason } => {
                ExitReason::Crashed {
                    stderr_tail: reason,
                }
            }
        };

        let request_records: Vec<ChatRequestRecord> = result
            .trajectory
            .iter()
            .enumerate()
            .map(|(turn, round)| ChatRequestRecord {
                turn: turn as u32,
                role: None,
                request: json!({
                    "search_round": round.round,
                    "candidates": round.candidates,
                    "details": round.details,
                }),
                response: json!({
                    "winner": round.winner,
                    "passing_after": round.passing_after,
                    "failed_after": round.failed_after,
                }),
                elapsed_ms: 0,
            })
            .collect();

        let final_assistant_text = format!(
            "Search summary: {}/{} tests passing after {} rounds.\n",
            result.tests_after.passed, result.tests_after.total, result.rounds
        );

        Ok(AgentRunArtifact {
            workdir: ctx.workdir,
            tokens: TokenCounts::default(),
            wall_ms: started.elapsed().as_millis() as u64,
            exit_reason,
            tool_calls: vec![],
            stderr_tail: String::new(),
            final_assistant_text,
            raw_stdout_lines: vec![],
            request_records,
            role_model_map_used: None,
        })
    }
}

fn crashed_artifact(
    workdir: tempfile::TempDir,
    started: Instant,
    stderr_tail: String,
) -> AgentRunArtifact {
    AgentRunArtifact {
        workdir,
        tokens: TokenCounts::default(),
        wall_ms: started.elapsed().as_millis() as u64,
        exit_reason: ExitReason::Crashed { stderr_tail },
        tool_calls: vec![],
        stderr_tail: String::new(),
        final_assistant_text: String::new(),
        raw_stdout_lines: vec![],
        request_records: vec![],
        role_model_map_used: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_runner_has_stable_id() {
        assert_eq!(SearchRunner::new().id(), "search");
    }

    #[test]
    fn search_runner_default_model_is_primary() {
        assert_eq!(
            SearchRunner::new().default_model_handle(),
            Some("commonwealth/primary")
        );
    }

    #[test]
    fn search_runner_accepts_custom_backend() {
        use commonwealth_tdd::DeterministicChatBackend;
        let _r = SearchRunner::with_backend(Arc::new(DeterministicChatBackend::from_strs(Vec::<
            String,
        >::new(
        ))));
    }
}
