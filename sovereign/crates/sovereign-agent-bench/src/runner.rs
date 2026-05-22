//! `AgentRunner` trait + run context + artifact.
//!
//! The seam between the bench harness and a concrete coding agent
//! (pi, opencode, codex, …). One async `run` method per ARCH §5.2.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use thiserror::Error;

use crate::problem::Problem;

/// Why the agent stopped emitting work. Closed enum per ARCH §2.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum ExitReason {
    /// Agent exited cleanly with a zero status.
    Completed,
    /// Cumulative output tokens crossed `token_budget`; harness sent SIGTERM.
    TokensExceeded { cap: u64, observed: u64 },
    /// Wall-clock cap fired before completion.
    Timeout { cap_seconds: u64 },
    /// Agent kept emitting tool calls without changing the workdir
    /// state — typically a read-loop on an empty directory under
    /// grammar-constrained tool sampling. Distinct from `Timeout` so
    /// the operator can tell "model thought too long" from "model
    /// stuck in a tool loop with nothing to do."
    NoProgress {
        consecutive_tool_calls: u32,
        threshold: u32,
    },
    /// Subprocess exited non-zero or died on a signal. `stderr_tail` carries
    /// up to 32 KiB of context.
    Crashed { stderr_tail: String },
    /// Agent attempted a tool not in the allowlist. Reserved for runners
    /// that enforce allowlist client-side.
    ToolDenied { tool: String },
    /// Agent issued N consecutive `write` tool calls without a `bash`
    /// verification step in between — the canonical write-thrash
    /// failure mode where each rewrite overlays partial code on top
    /// of partial code and final output is incoherent. Distinct from
    /// `NoProgress` (which fires when workdir is unchanged) because
    /// write-thrash DOES change the workdir, just incoherently.
    WriteThrash {
        consecutive_writes: u32,
        threshold: u32,
    },
    /// Build / smoke produced the same failing stdout_tail
    /// `hash_repeats` times in a row — the model cannot fix the
    /// error it keeps hitting. Closes the L5/L6 loop class (same
    /// compiler error or same test failure repeating across cycles).
    /// Successful verifications reset the counter, so a 1-line fix
    /// that re-surfaces the same error twice while productively
    /// whittling never trips this.
    VerifyStuck { hash_repeats: u32, threshold: u32 },
    /// Role-aware native runner observed `cycles` complete
    /// Implementer↔Evaluator round-trips (counted on
    /// `handoff_to_implementer`) without an `agent_done`. Hard
    /// ceiling on non-convergent alternation (L4 / L7 / L17).
    CycleLimit { cycles: u32, cap: u32 },
}

impl Default for ExitReason {
    fn default() -> Self {
        ExitReason::Completed
    }
}

impl ExitReason {
    pub fn id(&self) -> &'static str {
        match self {
            ExitReason::Completed => "completed",
            ExitReason::TokensExceeded { .. } => "tokens_exceeded",
            ExitReason::Timeout { .. } => "timeout",
            ExitReason::NoProgress { .. } => "no_progress",
            ExitReason::Crashed { .. } => "crashed",
            ExitReason::ToolDenied { .. } => "tool_denied",
            ExitReason::WriteThrash { .. } => "write_thrash",
            ExitReason::VerifyStuck { .. } => "verify_stuck",
            ExitReason::CycleLimit { .. } => "cycle_limit",
        }
    }

    pub fn is_completed(&self) -> bool {
        matches!(self, ExitReason::Completed)
    }
}

/// Token accounting for one agent run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCounts {
    /// Sum of input tokens across every model turn (prompt + history).
    pub input: u64,
    /// Sum of output tokens (new generation only) — the canonical
    /// "thinking" measure used by the budget enforcement path.
    pub output: u64,
}

/// One full chat-completion request + response captured during a
/// run. Persisted to `requests.jsonl` so the operator can replay a
/// specific turn through the `replay` subcommand with overrides
/// (different temperature, different model, edited messages, etc.).
/// The bench's iteration loop is fast, but replaying a single turn
/// is much faster — and lets you settle "is it the prompt or the
/// model?" debates without rerunning the whole bench.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequestRecord {
    pub turn: u32,
    /// Active role (e.g. "planner", "implementer", "evaluator") for
    /// role-aware runners; `None` for monolithic / pi runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Full POST body sent to /v1/chat/completions (messages, tools,
    /// tool_choice, sampling overrides).
    pub request: serde_json::Value,
    /// Full response body received from the daemon. Captured even on
    /// HTTP error (status + text recorded).
    pub response: serde_json::Value,
    /// Wall-clock time spent on this single request.
    pub elapsed_ms: u64,
}

/// One tool invocation observed during the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub turn: u32,
    pub tool: String,
    /// JSON-serialized argument body, capped to keep the report compact.
    pub args_preview: String,
    pub ok: bool,
    /// Canonical primitive kind this tool call maps to (via the
    /// agent's adapter). `None` for unrecognized agent tool calls,
    /// for reports written before the canonical layer existed, and
    /// for the `agent_done` virtual that doesn't go through a
    /// runner. Lets cross-agent rollups slice by canonical kind
    /// instead of agent-specific tool names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_kind: Option<commonwealth_agent_tools::PrimitiveKind>,
}

/// Read-only context handed to the runner. The runner sees the workdir
/// path but NOT the fixture path — structural invariant per ARCH §7.2.
pub struct AgentRunContext {
    pub problem_id: String,
    pub prompt: String,
    /// Owned TempDir; the runner uses `workdir.path()`. Moved to the
    /// artifact on completion so the witness can read what the agent
    /// wrote, then dropped (deleting the dir) at end of scope.
    pub workdir: TempDir,
    /// Allowlist exposed to the agent. Const reference because every
    /// problem in v0 shares the pi tool set.
    pub tool_allowlist: &'static [&'static str],
    /// Output-token cap. Harness SIGTERMs the agent when the cumulative
    /// output count exceeds this value.
    pub token_budget: u64,
    /// Wall-clock cap (whole seconds).
    pub wall_seconds_cap: u64,
    /// Daemon-side model id (`commonwealth/coder` etc.) the agent
    /// should bind. Runners pass this through to their underlying CLI.
    pub model_handle: String,
    /// Per-language build command bound for this problem (e.g.
    /// `cargo build 2>&1` for Rust). Native runner threads it into
    /// `ExecCtx.build_cmd`; pi runner threads it into the pi adapter
    /// so `BashIntent::Build` matches the actual invocation.
    pub build_cmd: String,
    /// Per-language smoke/integration-test command (e.g. `cargo
    /// test --quiet --test integration`). Same threading as
    /// build_cmd.
    pub verify_cmd: String,
    /// Optional pre-build syntax validator. Bench plugs in a
    /// language-appropriate impl (`RustSyntaxValidator` for
    /// `language = "Rust"`, etc.) at context construction. Native
    /// runner threads it into `ExecCtx.syntax_validator` so
    /// `exec_build` can short-circuit on broken syntax with cargo-
    /// shape feedback in <50ms instead of the full subprocess.
    pub syntax_validator: Option<commonwealth_agent_tools::syntax::DynSyntaxValidator>,
}

impl AgentRunContext {
    pub fn workdir(&self) -> &Path {
        self.workdir.path()
    }
}

/// What the runner produces. `workdir` carries ownership so the
/// witness pipeline can run real commands against the agent's
/// on-disk state.
pub struct AgentRunArtifact {
    pub workdir: TempDir,
    pub tokens: TokenCounts,
    pub wall_ms: u64,
    pub exit_reason: ExitReason,
    pub tool_calls: Vec<ToolCallRecord>,
    pub stderr_tail: String,
    /// Best-effort summary of the agent's final assistant message
    /// (for the judge prompt). May be empty when the agent never
    /// produced one (e.g. crashed mid-turn).
    pub final_assistant_text: String,
    /// Raw lines emitted on the agent's stdout — captured verbatim
    /// so the operator can reverse-engineer the agent's event
    /// schema when our parser missed something (empty tool_calls
    /// even though the agent did real work). Each entry is a single
    /// stdout line, no newline. Persisted to `agent.jsonl` by the
    /// artifact sink.
    pub raw_stdout_lines: Vec<String>,
    /// Captured chat-completion requests + responses, one per turn.
    /// Empty for runners that don't drive the daemon directly (pi).
    /// Persisted to `requests.jsonl` by the artifact sink so the
    /// `replay` subcommand can pick any turn and re-send it with
    /// overrides.
    pub request_records: Vec<ChatRequestRecord>,
}

impl AgentRunArtifact {
    pub fn workdir_path(&self) -> PathBuf {
        self.workdir.path().to_path_buf()
    }
}

#[derive(Debug, Error)]
pub enum AgentRunError {
    #[error("agent binary not found on PATH: {0}")]
    BinaryNotFound(String),
    #[error("agent subprocess spawn failed: {0}")]
    SpawnFailed(String),
    #[error("agent subprocess i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent runner internal error: {0}")]
    Internal(String),
}

/// The narrow trait per ARCH §5.2. Concrete runners implement this;
/// the registry hands them out by id.
#[async_trait]
pub trait AgentRunner: Send + Sync {
    /// Stable id (`"pi"`, `"opencode"`, `"mock"`).
    fn id(&self) -> &'static str;

    /// Model handle this runner will use by default. None when the
    /// runner is agent-only and doesn't pin a model.
    fn default_model_handle(&self) -> Option<&str> {
        None
    }

    /// Drive the agent on one problem. The runner owns:
    ///   - constructing the subprocess invocation
    ///   - parsing the per-turn telemetry
    ///   - enforcing `token_budget` and `wall_seconds_cap`
    ///   - returning a populated `AgentRunArtifact`
    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError>;
}

/// Convenience: build a context tied to a problem.
pub fn context_for(
    problem: &Problem,
    workdir: TempDir,
    tool_allowlist: &'static [&'static str],
    model_handle: String,
    token_budget_override: Option<u64>,
    wall_seconds_override: Option<u64>,
) -> AgentRunContext {
    use commonwealth_agent_tools::syntax::{DynSyntaxValidator, RustSyntaxValidator};
    use std::sync::Arc;
    let syntax_validator: Option<DynSyntaxValidator> = match problem.witness.language {
        crate::problem::WitnessLanguage::Rust => {
            Some(Arc::new(RustSyntaxValidator::new()) as DynSyntaxValidator)
        }
        // New languages plug their SyntaxValidator impl here. None
        // skips pre-build check; build subprocess still runs.
        crate::problem::WitnessLanguage::Go => None,
        crate::problem::WitnessLanguage::TypeScript => None,
        crate::problem::WitnessLanguage::Python => None,
    };
    AgentRunContext {
        problem_id: problem.meta.id.clone(),
        prompt: problem.prompt_text.clone(),
        workdir,
        tool_allowlist,
        token_budget: token_budget_override.unwrap_or(problem.budget.token_cap),
        wall_seconds_cap: wall_seconds_override.unwrap_or(problem.budget.wall_seconds_cap),
        model_handle,
        build_cmd: problem.witness.resolved_build_cmd(),
        verify_cmd: problem.witness.verify_cmd.clone(),
        syntax_validator,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::problem::{
        BudgetCfg, Category, Problem, ProblemMeta, PromptCfg, ScoringCfg, ScoringDimCfg,
        ScoringMode, Tier, WitnessCfg, WitnessKind, WitnessLanguage,
    };

    fn fake_problem() -> Problem {
        Problem {
            meta: ProblemMeta {
                id: "test-1".to_string(),
                title: "T".to_string(),
                category: Category::CodeTest,
                version: "v0".to_string(),
                notes: String::new(),
                tier: Tier::FromScratch,
            },
            prompt: PromptCfg {
                file: "prompt.md".to_string(),
            },
            witness: WitnessCfg {
                kind: WitnessKind::AutoTestPass,
                language: WitnessLanguage::Rust,
                fixture_subdir: "fixtures".to_string(),
                scaffold_subdir: None,
                verify_cmd: "true".to_string(),
                build_cmd: None,
                score_buckets: vec![[0.0, 1.001, 3.0]],
            },
            budget: BudgetCfg {
                token_cap: 100,
                wall_seconds_cap: 30,
            },
            scoring: ScoringCfg {
                dim_a: ScoringDimCfg {
                    name: "Correctness".into(),
                    mode: ScoringMode::AutoTestPassFraction,
                },
                dim_b: ScoringDimCfg {
                    name: "Approach".into(),
                    mode: ScoringMode::JudgeRubric {
                        rubric_id: "dim_b".into(),
                    },
                },
                dim_c: ScoringDimCfg {
                    name: "Efficiency".into(),
                    mode: ScoringMode::HybridAutoFloor {
                        rubric_id: "dim_c".into(),
                    },
                },
            },
            prompt_text: "do the thing".to_string(),
            rubric_anchors: Default::default(),
            problem_dir: PathBuf::new(),
        }
    }

    #[test]
    fn context_for_inherits_problem_budget() {
        let workdir = tempfile::tempdir().unwrap();
        let ctx = context_for(
            &fake_problem(),
            workdir,
            &["read"],
            "commonwealth/coder".to_string(),
            None,
            None,
        );
        assert_eq!(ctx.token_budget, 100);
        assert_eq!(ctx.wall_seconds_cap, 30);
    }

    #[test]
    fn context_for_respects_overrides() {
        let workdir = tempfile::tempdir().unwrap();
        let ctx = context_for(
            &fake_problem(),
            workdir,
            &["read"],
            "commonwealth/coder".to_string(),
            Some(8_000),
            Some(120),
        );
        assert_eq!(ctx.token_budget, 8_000);
        assert_eq!(ctx.wall_seconds_cap, 120);
    }

    #[test]
    fn exit_reason_ids_round_trip() {
        assert_eq!(ExitReason::Completed.id(), "completed");
        assert_eq!(
            ExitReason::TokensExceeded {
                cap: 1,
                observed: 2
            }
            .id(),
            "tokens_exceeded"
        );
        assert!(ExitReason::Completed.is_completed());
        assert!(!ExitReason::Timeout { cap_seconds: 1 }.is_completed());
    }
}
