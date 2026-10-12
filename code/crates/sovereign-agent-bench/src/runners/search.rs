// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search-not-agent runner — thin adapter over
//! [`sovereign_tdd::run_trial`].
//!
//! The actual solver loop lives in `sovereign-tdd::trial` (the
//! collapsed surface as of 2026-05-24). This module just maps the
//! bench's `AgentRunContext` → `sovereign_tdd::Trial` with
//! `Polarity::MaximizePassing` (the Green-equivalent default),
//! dispatches, and maps `TrialResult` back to `AgentRunArtifact`.
//! All loop semantics — parallel candidates, monotonic gating,
//! stall detection — are validated by `sovereign-tdd`'s own
//! test suite.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tracing::{info, warn};

use sovereign_tdd::{
    run_trial_observed, ChatBackend, Polarity, ReqwestChatBackend, RoundObserver, RoundSummary,
    Trial, TrialConfig, TrialStatus, Workdir,
};

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ChatRequestRecord, ExitReason,
    TokenCounts,
};

pub struct SearchRunner {
    backend: Arc<dyn ChatBackend>,
}

impl SearchRunner {
    pub(crate) fn new() -> Self {
        Self::with_provider_url("http://localhost:9741/v1".into())
    }

    pub(crate) fn with_provider_url(provider_url: String) -> Self {
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
/// sovereign-tdd Workdir gate accepts the bench's scratch dir.
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
            prompt: strip_delivery_section(&ctx.prompt),
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

        // The problem's wall cap binds search the way it binds pi and
        // opencode: stop and keep the workdir. Past it the bench's outer
        // watchdog substitutes an empty one. The canonical workdir only
        // changes when a round promotes its winner (a synchronous
        // `snapshot_dir`), and candidates live in a JoinSet that aborts
        // on drop, so cancelling here leaves the last promoted state on
        // disk. Rounds finished before the cap reach the artifact
        // through the observer.
        let rounds: Arc<Mutex<Vec<RoundSummary>>> = Arc::default();
        let observer: RoundObserver = {
            let rounds = Arc::clone(&rounds);
            Arc::new(move |r: &RoundSummary| {
                rounds
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(r.clone())
            })
        };
        let cap = Duration::from_secs(ctx.wall_seconds_cap);
        let trial_run = run_trial_observed(trial, Arc::clone(&self.backend), Some(observer));
        let result = match tokio::time::timeout(cap, trial_run).await {
            Ok(result) => result,
            Err(_) => {
                let trajectory = rounds.lock().unwrap_or_else(|e| e.into_inner()).clone();
                info!(
                    cap_secs = ctx.wall_seconds_cap,
                    rounds = trajectory.len(),
                    "search: wall cap reached — returning the last promoted workdir"
                );
                return Ok(AgentRunArtifact {
                    workdir: ctx.workdir,
                    tokens: TokenCounts::default(),
                    wall_ms: started.elapsed().as_millis() as u64,
                    exit_reason: ExitReason::Timeout {
                        cap_seconds: ctx.wall_seconds_cap,
                    },
                    tool_calls: vec![],
                    stderr_tail: String::new(),
                    final_assistant_text: format!(
                        "Search stopped at the {}s wall cap after {} completed rounds.\n",
                        ctx.wall_seconds_cap,
                        trajectory.len()
                    ),
                    raw_stdout_lines: vec![],
                    request_records: request_records(&trajectory),
                    role_model_map_used: None,
                });
            }
        };

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

        let request_records = request_records(&result.trajectory);

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

fn request_records(trajectory: &[RoundSummary]) -> Vec<ChatRequestRecord> {
    trajectory
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
        .collect()
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

    /// A backend that never answers within any cap a test sets.
    struct StalledBackend;

    #[async_trait]
    impl ChatBackend for StalledBackend {
        async fn complete(
            &self,
            _model: &str,
            _messages: Vec<serde_json::Value>,
            _temperature: f32,
            _max_tokens: u32,
        ) -> Result<sovereign_tdd::ChatResponse, sovereign_tdd::BackendError> {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            Err(sovereign_tdd::BackendError::Transport("stalled".into()))
        }
    }

    /// pi and opencode stop at the problem's wall cap and hand back the
    /// workdir they wrote. Search must do the same: past the cap the
    /// bench's outer watchdog substitutes an EMPTY workdir, which scores
    /// any progress the search made as zero.
    #[tokio::test]
    async fn search_stops_at_its_wall_cap_with_the_real_workdir() {
        let workdir = tempfile::tempdir().unwrap();
        std::fs::write(workdir.path().join("marker.txt"), "scaffold").unwrap();
        let ctx = AgentRunContext {
            problem_id: "cap".into(),
            prompt: "make b pass".into(),
            workdir,
            tool_allowlist: &[],
            token_budget: 1000,
            wall_seconds_cap: 1,
            model_handle: "m".into(),
            build_cmd: "true".into(),
            verify_cmd: "counts: printf 'PASS a\\nFAIL b\\n'".into(),
            syntax_validator: None,
            role_model_map: Default::default(),
            workdir_scale: sovereign_agent_tools::WorkdirScale::Scaffold,
        };
        let runner = SearchRunner::with_backend(Arc::new(StalledBackend));
        let artifact = tokio::time::timeout(std::time::Duration::from_secs(30), runner.run(ctx))
            .await
            .expect("search must stop at its own wall cap, not run past it")
            .expect("run");
        assert!(
            matches!(artifact.exit_reason, ExitReason::Timeout { cap_seconds: 1 }),
            "{:?}",
            artifact.exit_reason
        );
        assert_eq!(
            std::fs::read_to_string(artifact.workdir.path().join("marker.txt")).unwrap(),
            "scaffold"
        );
    }

    #[test]
    fn search_runner_accepts_custom_backend() {
        use sovereign_tdd::DeterministicChatBackend;
        let _r = SearchRunner::with_backend(Arc::new(DeterministicChatBackend::from_strs(Vec::<
            String,
        >::new(
        ))));
    }
}

/// The battery's prompt.md files end with a "## How to deliver"
/// section written for TOOL-CALLING runners ("use the write tool…
/// **Do NOT paste the solution into chat**"). Under the search
/// runner the delivery contract is the trial prompt's own — emit one
/// fenced JSON action plus one fenced source block, i.e. exactly
/// "paste the solution into chat". Forwarding the tool-agent section
/// verbatim hands a small model two contradictory delivery contracts
/// in one prompt. Delivery instructions belong to the RUNNER, not
/// the problem statement; cut the section, keep everything else.
fn strip_delivery_section(prompt: &str) -> String {
    const HEADING: &str = "## How to deliver";
    let Some(start) = prompt.find(HEADING) else {
        return prompt.to_string();
    };
    let tail = &prompt[start + HEADING.len()..];
    let mut out = String::with_capacity(prompt.len());
    out.push_str(prompt[..start].trim_end());
    out.push('\n');
    if let Some(i) = tail.find("\n## ") {
        out.push('\n');
        out.push_str(&tail[i + 1..]);
    }
    out
}

#[cfg(test)]
mod delivery_strip_tests {
    use super::strip_delivery_section;

    #[test]
    fn strips_trailing_delivery_section() {
        let p = "# Task\n\nBody text.\n\n## How to deliver\n\nUse the write tool.\n**Do NOT paste the solution into chat.**\n";
        let s = strip_delivery_section(p);
        assert!(!s.contains("How to deliver"));
        assert!(!s.contains("Do NOT paste"));
        assert!(s.contains("Body text."));
    }

    #[test]
    fn keeps_sections_after_a_mid_prompt_delivery_section() {
        let p = "# Task\n\n## How to deliver\n\nwrite tool stuff\n\n## Constraints\n\n- std only\n";
        let s = strip_delivery_section(p);
        assert!(!s.contains("write tool stuff"));
        assert!(s.contains("## Constraints"));
        assert!(s.contains("- std only"));
    }

    #[test]
    fn prompt_without_section_unchanged() {
        let p = "# Task\n\nJust a body.\n";
        assert_eq!(strip_delivery_section(p), p);
    }
}
