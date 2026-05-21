//! `sovereign agent-bench run` orchestration. Walks problems → runs
//! the agent on each → invokes the witness → judges → assembles a
//! `BenchReport` → optionally persists the baseline.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;
use tracing::{info, warn};

use std::time::Duration;

use crate::artifacts::ArtifactSink;
use crate::baseline::{write_dated_and_update_latest, read_latest, BaselineError};
use crate::cli::args::{ArgsError, RunArgs};
use crate::judge::{request_for_dimension, HttpJudgeClient, JudgeClient, JudgeError, JudgeRequest, JudgeTrialOutcome};
use crate::judge_multi::{aggregate, MultiTrialOutcome};
use crate::problem::{load_problem, Problem, ProblemLoadError, ScoringMode};
use crate::report::BenchReport;
use crate::runner::{context_for, AgentRunArtifact, AgentRunner};
use crate::runners::pi::PI_TOOL_ALLOWLIST;
use crate::runners::AgentRunnerRegistry;
use crate::sandbox::Sandbox;
use crate::scoring::{
    compute_regression, dim_from_auto, dim_from_hybrid, dim_from_judge, DimensionScore,
    ProblemScore, WitnessSummary,
};
use crate::witness::run_auto_witness;

const GROUP: &str = "agent-coding";

#[derive(Debug, Error)]
pub enum RunError {
    #[error("args: {0}")]
    Args(#[from] ArgsError),
    #[error("problem load: {0}")]
    Problem(#[from] ProblemLoadError),
    #[error("agent runner `{0}` not registered (available: {1})")]
    UnknownAgent(String, String),
    #[error("agent run: {0}")]
    AgentRun(#[from] crate::runner::AgentRunError),
    #[error("witness: {0}")]
    Witness(#[from] crate::witness::AutoWitnessError),
    #[error("judge: {0}")]
    Judge(#[from] JudgeError),
    #[error("baseline: {0}")]
    Baseline(#[from] BaselineError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no problems found under {0}")]
    NoProblems(String),
}

pub async fn run_command(argv: &[String]) -> Result<(), RunError> {
    let args = RunArgs::parse(argv)?;
    let started_at = Utc::now().to_rfc3339();

    let registry = AgentRunnerRegistry::builtin();
    let runner = registry.get(&args.agent).ok_or_else(|| {
        RunError::UnknownAgent(args.agent.clone(), registry.agent_ids().join(", "))
    })?;
    let runner = patch_runner_pi_binary(runner, &args);

    let problems = discover_problems(&args.bench_root, args.problems.as_deref())?;
    if problems.is_empty() {
        return Err(RunError::NoProblems(args.bench_root.display().to_string()));
    }

    let judge_model = args.judge_model.clone().unwrap_or_else(|| args.model.clone());
    let judge: Arc<dyn JudgeClient> = Arc::new(HttpJudgeClient::new(
        &args.judge_base_url,
        judge_model.clone(),
    ));

    // Resolve the run-wide artifacts root. Default to
    // `<bench_root>/.artifacts/<utc-date>-<agent>-<model-slug>/`.
    let artifacts_root = resolve_artifacts_root(&args);
    info!(
        path = %artifacts_root.display(),
        "agent_bench: artifacts root"
    );

    let mut scores: Vec<ProblemScore> = Vec::new();
    for problem in &problems {
        info!(
            problem = %problem.meta.id,
            agent = %args.agent,
            model = %args.model,
            budget = problem.budget.token_cap,
            "agent_bench: problem started"
        );
        let sink = ArtifactSink::new(artifacts_root.join(&problem.meta.id))
            .map_err(RunError::Io)?;
        let score = run_one_problem(
            &*runner,
            judge.as_ref(),
            problem,
            &args,
            Some(&sink),
        )
        .await?;
        info!(
            problem = %problem.meta.id,
            total = score.total,
            dim_a = score.dim_a.raw,
            dim_b = score.dim_b.raw,
            dim_c = score.dim_c.raw,
            wall_ms = score.wall_ms,
            tokens_out = score.tokens.output,
            exit = score.exit_reason.id(),
            partial = score.is_partial,
            "agent_bench: problem complete"
        );
        scores.push(score);
    }

    let finished_at = Utc::now().to_rfc3339();
    let grand_total = BenchReport::compute_grand_total(&scores);
    let max_total = (problems.len() as u16).saturating_mul(9);

    // Regression compare against the prior latest.json (read-only;
    // both `--update-baseline` and naked runs benefit from the
    // diagnostic).
    let prior: Option<BenchReport> = read_latest(&args.bench_root, GROUP)?;
    let regression = prior.map(|p| compute_regression(&scores, &p.per_problem, 1));

    if let Some(r) = &regression {
        if r.regressed {
            warn!(
                delta = r.delta,
                "agent_bench: regression detected vs latest.json"
            );
        } else {
            info!(delta = r.delta, "agent_bench: no_regression");
        }
    }

    let report = BenchReport {
        agent: args.agent.clone(),
        model: args.model.clone(),
        judge_model,
        judge_trials: args.judge_trials,
        started_at,
        finished_at,
        per_problem: scores,
        grand_total,
        max_total,
        regression,
    };

    // Always write --report.
    let report_bytes = serde_json::to_vec_pretty(&report)?;
    std::fs::write(&args.report_path, &report_bytes)?;
    info!(path = %args.report_path.display(), "agent_bench: report written");

    if args.update_baseline {
        let snapshot = write_dated_and_update_latest(
            &args.bench_root,
            GROUP,
            &report.agent,
            &report.model,
            &report,
        )?;
        info!(
            path = %snapshot.display(),
            "agent_bench: baseline updated"
        );
    }

    println!("{}", report.text_rollup());
    info!(
        grand_total,
        max_total,
        "agent_bench: grand_total"
    );
    Ok(())
}

/// Apply the optional `--pi-binary` flag to a freshly-resolved
/// runner. We can't mutate the trait object's underlying state via
/// the trait, so when the user pinned a specific pi binary we
/// replace the runner with a fresh `PiRunner` configured with it.
/// Other runners are returned unchanged.
fn patch_runner_pi_binary(
    runner: Arc<dyn AgentRunner>,
    args: &RunArgs,
) -> Arc<dyn AgentRunner> {
    if runner.id() != "pi" {
        return runner;
    }
    let mut pi = crate::runners::pi::PiRunner::new();
    if let Some(b) = args.pi_binary.clone() {
        pi = pi.with_binary(b);
    }
    pi = pi.with_provider_url(args.judge_base_url.clone());
    Arc::new(pi)
}

/// Run a single problem end-to-end against the supplied runner and
/// judge. Pub so integration tests can wire mock runners + stub
/// judges without spawning subprocesses. Pass `sink = None` to skip
/// artifact persistence (useful for the integration test).
pub async fn run_one_problem(
    runner: &dyn AgentRunner,
    judge: &dyn JudgeClient,
    problem: &Problem,
    args: &RunArgs,
    sink: Option<&ArtifactSink>,
) -> Result<ProblemScore, RunError> {
    // 1. Build sandbox. Scaffolded tier — install the pre-supplied
    // Cargo.toml + src/lib.rs stub BEFORE handing control to the
    // agent. From-scratch tier — workdir stays empty.
    let sandbox = Sandbox::new(problem.fixture_path())?;
    if let Some(scaffold) = problem.scaffold_path() {
        sandbox.install_scaffold(&scaffold)?;
        info!(
            problem = %problem.meta.id,
            scaffold = %scaffold.display(),
            tier = problem.meta.tier.id(),
            "agent_bench: scaffold installed"
        );
    } else if matches!(problem.meta.tier, crate::problem::Tier::Scaffolded) {
        warn!(
            problem = %problem.meta.id,
            "agent_bench: tier=Scaffolded but no scaffold_subdir set — agent sees empty workdir"
        );
    }
    let (workdir, _fixture_path) = sandbox.into_workdir();
    let ctx = context_for(
        problem,
        workdir,
        PI_TOOL_ALLOWLIST,
        args.model.clone(),
        args.token_cap_override,
        args.wall_seconds_override,
    );

    // 2. Run the agent.
    let artifact = runner.run(ctx).await?;

    // 2b. Persist agent artifacts (workdir copy + run summary). Do
    // this BEFORE witness so a witness failure can't lose the
    // agent's evidence.
    if let Some(sink) = sink {
        if let Err(e) = sink.persist_agent_run(&artifact) {
            warn!(error = %e, problem = %problem.meta.id, "agent_bench: artifact persist (agent) failed");
        }
    }

    // 3. Witness — runs regardless of exit_reason.
    let workdir_path = artifact.workdir.path().to_path_buf();
    let witness = match run_auto_witness(problem, &workdir_path).await {
        Ok(w) => Some(w),
        Err(e) => {
            warn!(error = %e, problem = %problem.meta.id, "agent_bench: witness failed");
            None
        }
    };

    // 3c. Memory headroom: when the agent and judge use different
    // slots, give the daemon's idle monitor a chance to unload the
    // agent's slot before the judge loads a 29 GB primary slot on
    // top of it. On a 64 GB Mac, having both hot peaks RSS into the
    // jetsam zone (observed 52 GB → SIGTERM in run o). Combined with
    // `extras_idle_secs=30` in setup_config, this sleep lets fast/
    // coder unload before the judge's first turn. When agent and
    // judge share a slot, the sleep is wasted but cheap.
    let judge_model = args
        .judge_model
        .clone()
        .unwrap_or_else(|| args.model.clone());
    if needs_slot_swap(&args.model, &judge_model) {
        info!(
            problem = %problem.meta.id,
            agent_model = %args.model,
            judge_model = %judge_model,
            "agent_bench: sleeping pre-judge to let agent slot idle out"
        );
        tokio::time::sleep(Duration::from_secs(35)).await;
    }
    // 3b. Persist post-witness workdir (held-out tests + verify
    // outputs are now in the workdir; useful for debugging witness
    // failures).
    if let Some(sink) = sink {
        let post_witness_dst = sink.root().join("workdir-post-witness");
        let _ = std::fs::create_dir_all(&post_witness_dst);
        let _ = copy_dir_filtered(&workdir_path, &post_witness_dst);
        if let Some(w) = witness.as_ref() {
            let summary = serde_json::json!({
                "verify_exit_ok": w.verify_exit_ok,
                "stdout_tail": w.stdout_tail,
                "passed": w.parsed.passed,
                "failed": w.parsed.failed,
                "total": w.parsed.total,
                "failed_names": w.parsed.failed_names,
                "pass_fraction": w.parsed.pass_fraction(),
                "bucketed_score": w.bucketed_score,
            });
            let body = serde_json::to_vec_pretty(&summary).unwrap_or_default();
            let _ = std::fs::write(sink.root().join("witness.json"), body);
        }
    }

    // 4. Score per dimension.
    let dim_a = score_dim(
        &problem.scoring.dim_a.mode,
        witness.as_ref(),
        problem,
        &artifact,
        "dim_a",
        &problem.scoring.dim_a.name,
        judge,
        args.judge_trials,
        sink,
    )
    .await?;
    let dim_b = score_dim(
        &problem.scoring.dim_b.mode,
        witness.as_ref(),
        problem,
        &artifact,
        "dim_b",
        &problem.scoring.dim_b.name,
        judge,
        args.judge_trials,
        sink,
    )
    .await?;
    let dim_c = score_dim(
        &problem.scoring.dim_c.mode,
        witness.as_ref(),
        problem,
        &artifact,
        "dim_c",
        &problem.scoring.dim_c.name,
        judge,
        args.judge_trials,
        sink,
    )
    .await?;

    let total = ProblemScore::compute_total(dim_a.raw, dim_b.raw, dim_c.raw);
    let is_partial = !artifact.exit_reason.is_completed();
    let witness_summary = witness.as_ref().map(WitnessSummary::from_outcome);
    Ok(ProblemScore {
        problem_id: problem.meta.id.clone(),
        dim_a,
        dim_b,
        dim_c,
        total,
        exit_reason: artifact.exit_reason,
        tokens: artifact.tokens,
        wall_ms: artifact.wall_ms,
        tool_calls: artifact.tool_calls,
        witness_summary,
        is_partial,
    })
}

#[allow(clippy::too_many_arguments)]
async fn score_dim(
    mode: &ScoringMode,
    witness: Option<&crate::witness::AutoWitnessOutcome>,
    problem: &Problem,
    artifact: &AgentRunArtifact,
    dim_id: &str,
    dim_name: &str,
    judge: &dyn JudgeClient,
    judge_trials: u8,
    sink: Option<&ArtifactSink>,
) -> Result<DimensionScore, RunError> {
    match mode {
        ScoringMode::AutoTestPassFraction => {
            let (frac, bucketed, verify_ok) = match witness {
                Some(w) => (
                    w.parsed.pass_fraction(),
                    w.bucketed_score,
                    w.verify_exit_ok,
                ),
                None => (0.0, 0, false),
            };
            Ok(dim_from_auto(frac, bucketed, verify_ok))
        }
        ScoringMode::JudgeRubric { rubric_id } => {
            let req = request_for_dimension(problem, artifact, dim_name, rubric_id)?;
            let outcome = run_judge_trials_glassbox(
                judge, &req, judge_trials, dim_id, sink,
            )
            .await;
            Ok(dim_from_judge(&outcome))
        }
        ScoringMode::HybridAutoFloor { rubric_id } => {
            let auto_score = witness.map(|w| w.bucketed_score).unwrap_or(0);
            let req = request_for_dimension(problem, artifact, dim_name, rubric_id)?;
            let outcome = run_judge_trials_glassbox(
                judge, &req, judge_trials, dim_id, sink,
            )
            .await;
            Ok(dim_from_hybrid(auto_score, &outcome))
        }
    }
}

/// Drive `N` judge trials with per-trial artifact persistence and
/// resilient error handling. A failed trial logs the error, persists
/// the failure shape to disk, and contributes a 0-anchor outcome to
/// the aggregator — the score reflects "judge unavailable" honestly
/// instead of silently dropping the dimension.
async fn run_judge_trials_glassbox(
    judge: &dyn JudgeClient,
    req: &JudgeRequest,
    trials: u8,
    dim_id: &str,
    sink: Option<&ArtifactSink>,
) -> MultiTrialOutcome {
    let trials = trials.max(1);
    let mut outcomes: Vec<JudgeTrialOutcome> = Vec::with_capacity(trials as usize);
    for i in 0..trials {
        match judge.judge(req).await {
            Ok(outcome) => {
                if let Some(s) = sink {
                    let _ = s.persist_judge_trial(dim_id, i, req, Ok(&outcome));
                }
                outcomes.push(outcome);
            }
            Err(e) => {
                tracing::warn!(
                    dim = dim_id,
                    trial = i,
                    error = %e,
                    "agent_bench: judge trial failed — scored 0"
                );
                if let Some(s) = sink {
                    let _ = s.persist_judge_trial(dim_id, i, req, Err(&e));
                }
                outcomes.push(JudgeTrialOutcome {
                    anchor: 0,
                    rationale: format!("judge unavailable: {e}"),
                });
            }
        }
    }
    aggregate(outcomes, trials)
}

/// Heuristic: when does the harness need to wait for slot turnover
/// between agent and judge? Returns true when the canonical handles
/// differ — e.g. agent=commonwealth/fast, judge=commonwealth/primary
/// → different physical GGUFs → both can't stay hot under jetsam.
/// Same handle (or substrings that collapse) → no sleep needed.
fn needs_slot_swap(agent_model: &str, judge_model: &str) -> bool {
    let a = canonical_slot(agent_model);
    let j = canonical_slot(judge_model);
    a != j
}

fn canonical_slot(handle: &str) -> &str {
    // Strip the `commonwealth/` namespace so `commonwealth/fast` and
    // `fast` collapse to the same slot identity.
    handle.strip_prefix("commonwealth/").unwrap_or(handle)
}

fn resolve_artifacts_root(args: &RunArgs) -> std::path::PathBuf {
    if let Some(d) = &args.artifacts_dir {
        return d.clone();
    }
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let model_slug = args
        .model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect::<String>();
    args.bench_root
        .join(".artifacts")
        .join(format!("{date}-{agent}-{model_slug}", agent = args.agent))
}

/// Recursive copy of `src` into `dst`, skipping common build-output
/// directories. Used to persist the post-witness workdir for forensic
/// inspection.
fn copy_dir_filtered(src: &Path, dst: &Path) -> std::io::Result<()> {
    const SKIP: &[&str] = &["target", "node_modules", ".git", "__pycache__"];
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ft.is_dir() && SKIP.iter().any(|s| *s == name_str) {
            continue;
        }
        let target = dst.join(&name);
        if ft.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_dir_filtered(&entry.path(), &target)?;
        } else if ft.is_file() {
            let bytes = std::fs::read(entry.path()).unwrap_or_default();
            if bytes.iter().take(4096).any(|b| *b == 0) {
                continue;
            }
            std::fs::write(&target, &bytes)?;
        }
    }
    Ok(())
}

pub fn list_problems(argv: &[String]) -> Result<(), RunError> {
    let args = RunArgs::parse(argv)?;
    let problems = discover_problems(&args.bench_root, None)?;
    for p in &problems {
        println!(
            "{:<32} {:<14} {}",
            p.meta.id,
            p.meta.category.id(),
            p.witness.language.id()
        );
    }
    Ok(())
}

pub fn show_problem(argv: &[String]) -> Result<(), RunError> {
    let problem_id = argv
        .first()
        .cloned()
        .ok_or_else(|| RunError::NoProblems("expected one positional problem id".into()))?;
    let other = &argv[1..];
    let args = RunArgs::parse(other)?;
    // Resolve `3.2` → `3.2-lights-out` etc. via the same prefix
    // matcher used by `--problems`.
    let problems = discover_problems(&args.bench_root, Some(&[problem_id.clone()]))?;
    let problem = problems.into_iter().next().ok_or_else(|| {
        RunError::NoProblems(format!(
            "no problem under {} matched `{problem_id}`",
            args.bench_root.display()
        ))
    })?;
    println!("# {} — {}", problem.meta.id, problem.meta.title);
    println!(
        "category: {}    language: {}    version: {}",
        problem.meta.category.id(),
        problem.witness.language.id(),
        problem.meta.version
    );
    println!();
    println!("{}", problem.prompt_text);
    Ok(())
}

fn discover_problems(
    bench_root: &Path,
    filter: Option<&[String]>,
) -> Result<Vec<Problem>, RunError> {
    let problems_dir = bench_root.join("problems");
    if !problems_dir.is_dir() {
        return Err(RunError::NoProblems(bench_root.display().to_string()));
    }
    let mut ids: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&problems_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                if entry.path().join("problem.toml").is_file() {
                    ids.push(name.to_string());
                }
            }
        }
    }
    ids.sort();
    if let Some(filter) = filter {
        ids.retain(|id| filter.iter().any(|f| ids_match(id, f)));
    }
    let mut out: Vec<Problem> = Vec::new();
    for id in ids {
        let p = load_problem(&problems_dir.join(&id))?;
        out.push(p);
    }
    Ok(out)
}

/// `1.1` matches `1.1-regex-shortest-path` (next char is `-`). We
/// accept either the bare problem-id prefix or the full directory
/// name. We do NOT accept `3` matching `3.2-lights-out` — typing one
/// digit shouldn't fan out to every problem in that part of the
/// battery.
fn ids_match(dir_name: &str, query: &str) -> bool {
    if dir_name == query {
        return true;
    }
    if dir_name.starts_with(query) {
        let next = dir_name.as_bytes().get(query.len()).copied();
        match next {
            None => return true,
            Some(b) => {
                if b == b'-' || b == b'_' {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_accepts_numeric_prefix() {
        assert!(ids_match("1.1-regex-shortest-path", "1.1"));
        assert!(ids_match("3.2-lights-out", "3.2"));
        assert!(!ids_match("3.2-lights-out", "3"));
        assert!(!ids_match("3.20-foo", "3.2"));
        assert!(ids_match("3.2-lights-out", "3.2-lights-out"));
    }
}
