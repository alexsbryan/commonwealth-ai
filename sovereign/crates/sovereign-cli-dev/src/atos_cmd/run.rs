//! `sovereign atos run` — the ralph-wiggum-style loop driver.
//!
//! Reads `DESIGN.md` + `CHARTER.md` + `IMPLEMENTATION_PLAN.md` from the
//! workdir, spawns `opencode` (or `claude`), and keeps re-spawning
//! until either `DONE.md` is written and accepted by a reviewer pass,
//! `--max-iters` is hit, or the operator interrupts.
//!
//! The runner is operator-facing. It does not register MCP tools and
//! does not modify schemas; it composes existing pieces:
//!
//! - `sovereign-atos` orchestrator for run/feature lifecycle
//! - `Driver::spawn` shape from `milestone.rs` for subprocess fan-out
//! - `/v1/chat/completions` against the local Bench_Darwin slot for
//!   the reviewer judge (same call shape as `sovereign-eval`'s judge)
//!
//! See `sovereign/docs/ATOS_RUNNER.md` for the design rationale.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use corpus_engine::{FeatureStore, NoteStore};
use serde::{Deserialize, Serialize};
use sovereign_atos::{AtosOrchestrator, LocalAtosOrchestrator, RunMode};
use sovereign_tools::code::atos_utils::{
    detect_hollow_files, detect_missing_scaffold, detect_untouched_files, extract_verify_cmd,
    is_weak_verify, parse_inline_list, run_verify_cmd, sha256_hex, snapshot_file_mtimes,
    split_state_marker, step_goal_is_scaffold, strip_failure_cruft, truncate,
};

use super::args::{get_flag, split_args};

const DEFAULT_MAX_ITERS: u32 = 20;
const DEFAULT_DAEMON_URL: &str = "http://localhost:9741";
const DEFAULT_REVIEWER_MODEL: &str = "commonwealth/primary";
const DEFAULT_DRIVER_MODEL: &str = "commonwealth/primary";
const DEFAULT_DONE_MARKER: &str = "DONE.md";
const REVIEWER_MAX_TOKENS: u32 = 8192;
const REVIEWER_TEMPERATURE: f32 = 0.0;
const REVIEWER_TIMEOUT_S: u64 = 600;
const DIFF_BYTE_BUDGET: usize = 64 * 1024;

// ─── Public entry ────────────────────────────────────────────────────────────

pub(crate) async fn cmd_run(args: &[String]) -> i32 {
    let cfg = match RunCfg::from_args(args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("atos run: {msg}");
            print_help();
            return 2;
        }
    };

    if let Err(e) = cfg.validate() {
        eprintln!("atos run: {e}");
        return 2;
    }

    if cfg.show_help {
        print_help();
        return 0;
    }

    match drive(cfg).await {
        Ok(outcome) => match outcome {
            LoopOutcome::Accepted => 0,
            LoopOutcome::ExhaustedIterations => 1,
            LoopOutcome::ReviewerFailure => 1,
            LoopOutcome::Stuck => 1,
        },
        Err(e) => {
            eprintln!("atos run: {e}");
            1
        }
    }
}

// ─── Configuration ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct RunCfg {
    workdir: PathBuf,
    design_path: Option<PathBuf>,
    charter_path: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    feature_id: Option<String>,
    driver: DriverKind,
    driver_model: String,
    max_iters: u32,
    daemon_url: String,
    reviewer_model: String,
    done_marker: String,
    dry_run: bool,
    fresh_plan: bool,
    show_help: bool,
    /// Operator override per ATOS_RUNNER.md § Stop conditions #3.
    /// When set, the loop body short-circuits: the current workdir
    /// state is recorded as `verdict: "operator_accept"` and the
    /// orchestrator closes the run as accepted without spawning a
    /// driver or invoking the reviewer.
    accept: bool,
}

#[derive(Debug, Clone, Copy)]
enum DriverKind {
    Opencode,
    Claude,
    Codex,
}

impl DriverKind {
    fn label(self) -> &'static str {
        match self {
            Self::Opencode => "opencode",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

impl RunCfg {
    fn from_args(args: &[String]) -> Result<Self, String> {
        let (positional, flags) = split_args(args);
        let show_help = positional.iter().any(|p| p == "help" || p == "--help" || p == "-h");

        let workdir = match get_flag(&flags, "--workdir") {
            Some(s) => PathBuf::from(s),
            None => return Err("missing --workdir <path>".into()),
        };

        let driver = match get_flag(&flags, "--driver").as_deref() {
            None | Some("") | Some("opencode") => DriverKind::Opencode,
            Some("claude") => DriverKind::Claude,
            Some("codex") => DriverKind::Codex,
            Some(other) => return Err(format!(
                "unknown --driver '{other}' (use opencode|claude|codex)"
            )),
        };

        let max_iters = match get_flag(&flags, "--max-iters") {
            Some(s) => s
                .parse::<u32>()
                .map_err(|_| format!("--max-iters not a positive integer: {s}"))?,
            None => DEFAULT_MAX_ITERS,
        };
        if max_iters == 0 {
            return Err("--max-iters must be > 0".into());
        }

        let daemon_url = get_flag(&flags, "--daemon-url")
            .unwrap_or_else(|| DEFAULT_DAEMON_URL.to_string());

        let reviewer_model = get_flag(&flags, "--reviewer-model")
            .unwrap_or_else(|| DEFAULT_REVIEWER_MODEL.to_string());

        let driver_model = get_flag(&flags, "--driver-model")
            .unwrap_or_else(|| DEFAULT_DRIVER_MODEL.to_string());

        let done_marker = get_flag(&flags, "--done-marker")
            .unwrap_or_else(|| DEFAULT_DONE_MARKER.to_string());

        let dry_run = flags.iter().any(|(k, _)| k == "dry-run");
        let fresh_plan = flags.iter().any(|(k, _)| k == "fresh-plan");
        let accept = flags.iter().any(|(k, _)| k == "accept");

        let resolve_against_workdir = |raw: String| -> PathBuf {
            let p = PathBuf::from(&raw);
            if p.is_absolute() { p } else { workdir.join(p) }
        };

        Ok(Self {
            design_path: get_flag(&flags, "--design").map(resolve_against_workdir),
            charter_path: get_flag(&flags, "--charter").map(resolve_against_workdir),
            plan_path: get_flag(&flags, "--plan").map(resolve_against_workdir),
            workdir,
            feature_id: get_flag(&flags, "--feature-id"),
            driver,
            driver_model,
            max_iters,
            daemon_url,
            reviewer_model,
            done_marker,
            dry_run,
            fresh_plan,
            show_help,
            accept,
        })
    }

    fn validate(&self) -> Result<(), String> {
        if self.show_help {
            return Ok(());
        }
        if !self.workdir.is_dir() {
            return Err(format!("--workdir not a directory: {}", self.workdir.display()));
        }
        for (label, p) in [
            ("--design", &self.design_path),
            ("--charter", &self.charter_path),
            ("--plan", &self.plan_path),
        ] {
            if let Some(path) = p {
                if !path.exists() {
                    return Err(format!("{label} path missing: {}", path.display()));
                }
            }
        }
        Ok(())
    }
}

// ─── Resolved artifacts (auto-discovery) ─────────────────────────────────────

#[derive(Debug, Clone)]
struct ResolvedArtifacts {
    design: Option<NamedDoc>,
    charter: Option<NamedDoc>,
    plan: Option<NamedDoc>,
}

#[derive(Debug, Clone)]
struct NamedDoc {
    label: String,
    path: PathBuf,
    body: String,
}

fn resolve_artifacts(cfg: &RunCfg) -> Result<ResolvedArtifacts, String> {
    fn read_named(path: &Path) -> Result<NamedDoc, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        Ok(NamedDoc {
            label: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "doc".into()),
            path: path.to_path_buf(),
            body,
        })
    }
    fn first_existing(workdir: &Path, names: &[&str]) -> Option<PathBuf> {
        names
            .iter()
            .map(|n| workdir.join(n))
            .find(|p| p.is_file())
    }

    let design = match cfg.design_path.as_ref() {
        Some(p) => Some(read_named(p)?),
        None => first_existing(
            &cfg.workdir,
            &["DESIGN.md", "design.md", "ARCHITECTURE.md", "oicp-v0.3.md"],
        )
        .map(|p| read_named(&p))
        .transpose()?,
    };

    let charter = match cfg.charter_path.as_ref() {
        Some(p) => Some(read_named(p)?),
        None => first_existing(&cfg.workdir, &["CHARTER.md", ".sovereign/CHARTER.md"])
            .map(|p| read_named(&p))
            .transpose()?,
    };

    let plan = match cfg.plan_path.as_ref() {
        Some(p) => Some(read_named(p)?),
        None => first_existing(&cfg.workdir, &["IMPLEMENTATION_PLAN.md", "PLAN.md"])
            .map(|p| read_named(&p))
            .transpose()?,
    };

    if design.is_none() && charter.is_none() && plan.is_none() {
        return Err(
            "no DESIGN.md / CHARTER.md / IMPLEMENTATION_PLAN.md found in workdir; \
             pass --design / --charter / --plan explicitly or author one of those files"
                .into(),
        );
    }

    Ok(ResolvedArtifacts { design, charter, plan })
}

// ─── Plan-execute FSM types ──────────────────────────────────────────────────

/// One step in the agent-authored plan. Each step is a discrete unit
/// of work bounded by an executable `verify_cmd`. The runner refuses
/// to mark a step `Done` until the verify command exits zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Step {
    id: String,
    /// Short goal description. Required on first appearance in a
    /// plan; may be empty in a REASSESS-emitted step (the runner
    /// merge step fills it in from the prior plan when the agent
    /// only intended to change verify_cmd or files_touched). Steps
    /// added by REASSESS that don't appear in the prior plan must
    /// include `goal` — validation catches that.
    #[serde(default)]
    goal: String,
    #[serde(default)]
    files_touched: Vec<String>,
    /// Shell command that proves the step is done. Same default
    /// rules as `goal` — REASSESS may omit when carrying a step
    /// over unchanged; runner merges from prior plan.
    #[serde(default)]
    verify_cmd: String,
    #[serde(default)]
    rationale: String,
    #[serde(default = "default_step_state")]
    state: StepState,
    #[serde(default)]
    attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_verify_stdout: Option<String>,
}

fn default_step_state() -> StepState {
    StepState::Pending
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepState {
    Pending,
    InProgress,
    Done,
    Failed,
    Skipped,
}

/// The agent-authored plan. Lives at
/// `~/.sovereign/runs/<run-id>/plan.json` and is the source of truth
/// for what work remains. The runner mutates `state` / `attempts`
/// in place after each EXECUTE phase; the agent rewrites the plan
/// (with `revision` bumped) during REASSESS.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Plan {
    #[serde(default = "default_schema_version")]
    schema_version: String,
    /// Feature id this plan implements. Defaults so REASSESS agents
    /// that emit only the structural fields (steps + revision) still
    /// produce parseable plans — the FSM merge step refills this from
    /// the prior plan when the agent omits it.
    #[serde(default)]
    feature_id: String,
    #[serde(default)]
    design_sha: String,
    #[serde(default)]
    created_at: String,
    #[serde(default = "default_revision")]
    revision: u32,
    steps: Vec<Step>,
}

fn default_schema_version() -> String {
    "1".to_string()
}
fn default_revision() -> u32 {
    1
}

impl Plan {
    fn validate(&self) -> Result<(), String> {
        if self.feature_id.trim().is_empty() {
            return Err("plan has empty feature_id (cannot be defaulted on first plan)".into());
        }
        if self.steps.is_empty() {
            return Err("plan has zero steps — at least one is required".into());
        }
        if self.steps.len() > 32 {
            return Err(format!(
                "plan has {} steps; cap is 32 — agent should consolidate",
                self.steps.len()
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for step in &self.steps {
            if step.id.is_empty() {
                return Err("step has empty id".into());
            }
            if !seen.insert(&step.id) {
                return Err(format!("duplicate step id: {}", step.id));
            }
            if step.goal.trim().is_empty() {
                return Err(format!("step {} has empty goal", step.id));
            }
            if step.verify_cmd.trim().is_empty() {
                return Err(format!(
                    "step {} has empty verify_cmd; strict-verify mode requires every step to have a runnable shell gate",
                    step.id
                ));
            }
            if is_weak_verify(&step.verify_cmd, step_goal_is_scaffold(&step.goal)) {
                return Err(format!(
                    "step {} has a weak verify: `{}`. For Rust: add `-- test_name` filter after `--test` (e.g. `cargo test --test test_foo -- roundtrip`). \
                     For scaffold steps, use `cargo build -p <crate>` instead of bare `cargo test --test`. Weak verifies pass even when the test file is empty — the FSM credits forward motion that didn't happen.",
                    step.id, step.verify_cmd
                ));
            }
            // Reject non-ASCII characters in verify commands and file
            // paths. Observed with bilingual models (Qwen-based 35B):
            // Chinese characters leak into --test names (硬, 测试),
            // producing shell commands that can't resolve to real files.
            // Shell commands and filesystem paths MUST be ASCII.
            if step.verify_cmd.chars().any(|c| !c.is_ascii()) {
                return Err(format!(
                    "step {} verify_cmd contains non-ASCII characters: `{}`. Shell commands must be ASCII-only — use English identifiers for test names, file paths, and flags.",
                    step.id, step.verify_cmd
                ));
            }
            for f in &step.files_touched {
                if f.chars().any(|c| !c.is_ascii()) {
                    return Err(format!(
                        "step {} files_touched contains non-ASCII path: `{f}`. File paths must be ASCII-only.",
                        step.id
                    ));
                }
                // Also reject files_touched entries that look like
                // prose descriptions rather than actual file paths
                // (observed: "tests for forward-compat deserialization
                // of older peers' JSON"). Real file paths don't contain
                // spaces or start with lowercase prose.
                if f.split_ascii_whitespace().count() > 3 {
                    return Err(format!(
                        "step {} files_touched entry looks like prose, not a file path: `{f}`. Use actual relative file paths (e.g. `tests/test_foo.rs`), not descriptions.",
                        step.id
                    ));
                }
            }
        }
        Ok(())
    }

    fn next_pending(&self) -> Option<&Step> {
        // Failed-but-retryable steps (attempts < 2) take precedence
        // over fresh pending — we want to clear blockers before
        // moving on. Skipped/Done steps are inert.
        self.steps
            .iter()
            .find(|s| s.state == StepState::Failed && s.attempts < 2)
            .or_else(|| self.steps.iter().find(|s| s.state == StepState::Pending))
    }

    fn all_done(&self) -> bool {
        self.steps
            .iter()
            .all(|s| matches!(s.state, StepState::Done | StepState::Skipped))
    }

    fn any_blocked(&self) -> bool {
        self.steps
            .iter()
            .any(|s| s.state == StepState::Failed && s.attempts >= 2)
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| format!("serialize plan: {e}"))?;
        std::fs::write(path, body)
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    fn load(path: &Path) -> Result<Self, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let plan: Self = serde_json::from_str(&body)
            .map_err(|e| format!("parse plan {}: {e}", path.display()))?;
        plan.validate()?;
        Ok(plan)
    }
}

/// Phase the FSM picks for the next iteration. Decided from the
/// (plan, last_rejection, steps_since_reassess) tuple.
#[derive(Debug, Clone)]
enum Phase {
    Plan,
    Execute(String /* step id */),
    Reassess(ReassessTrigger),
    Final,
    Stuck,
}

#[derive(Debug, Clone)]
enum ReassessTrigger {
    Cadence,
    StepFailure,
    ReviewerReject,
}

fn decide_phase(
    plan: Option<&Plan>,
    last_rejection: Option<&RejectionMemo>,
    steps_since_reassess: u32,
    reassess_every_k: u32,
    just_failed_step: bool,
) -> Phase {
    let Some(plan) = plan else {
        return Phase::Plan;
    };
    if plan.any_blocked() {
        // A step has failed too many times. Reassess once more to
        // give the agent a chance to rewrite verify_cmd or split
        // the step. If reassess can't unstick, the FSM will report
        // Stuck on the next decision.
        if last_rejection.is_some() {
            return Phase::Stuck;
        }
        return Phase::Reassess(ReassessTrigger::StepFailure);
    }
    if just_failed_step {
        return Phase::Reassess(ReassessTrigger::StepFailure);
    }
    if last_rejection.is_some() {
        return Phase::Reassess(ReassessTrigger::ReviewerReject);
    }
    if steps_since_reassess >= reassess_every_k {
        return Phase::Reassess(ReassessTrigger::Cadence);
    }
    if plan.all_done() {
        return Phase::Final;
    }
    if let Some(next) = plan.next_pending() {
        return Phase::Execute(next.id.clone());
    }
    Phase::Final
}

// ─── The loop ────────────────────────────────────────────────────────────────

enum LoopOutcome {
    Accepted,
    ExhaustedIterations,
    ReviewerFailure,
    Stuck,
}

async fn drive(cfg: RunCfg) -> Result<LoopOutcome, String> {
    if cfg.show_help {
        print_help();
        return Ok(LoopOutcome::Accepted);
    }

    let artifacts = resolve_artifacts(&cfg)?;
    println!("atos run: workdir = {}", cfg.workdir.display());
    for (label, doc) in [
        ("design", &artifacts.design),
        ("charter", &artifacts.charter),
        ("plan", &artifacts.plan),
    ] {
        match doc {
            Some(d) => println!("  {label:>8} = {} ({} bytes)", d.path.display(), d.body.len()),
            None => println!("  {label:>8} = (none)"),
        }
    }

    // Charter-defined hard gate per ATOS_RUNNER.md § Stop conditions.
    // Pre-parsed once from the charter body. If present, the loop runs
    // this shell command after DONE.md is written each iteration; on
    // non-zero exit the reviewer is skipped and the iteration is
    // soft-rejected.
    let stop_condition: Option<String> = artifacts
        .charter
        .as_ref()
        .and_then(|c| sovereign_atos::charter::find_stop_condition(&c.body));
    if let Some(cmd) = stop_condition.as_deref() {
        println!("  stop_cond = `{cmd}` (gates reviewer)");
    }

    let orc = open_orchestrator_for(&cfg.workdir)
        .map_err(|e| format!("open .sovereign stores under {}: {e}", cfg.workdir.display()))?;
    let feature_id = resolve_or_provision_feature(&orc, &cfg, &artifacts).await?;

    // Open a single run row that spans all iterations. Per-iteration
    // detail goes in iterations.jsonl on disk; the atos_runs row is
    // the audit anchor.
    let milestones = orc
        .list_milestones(&feature_id)
        .await
        .map_err(|e| format!("list milestones for {feature_id}: {e}"))?;
    let milestone_id = match milestones.first() {
        Some(m) => m.id.clone(),
        None => {
            // No milestones (synthetic feature) — synthesize one so the
            // run row has something to reference.
            let m = orc
                .add_milestone(&feature_id, 1, "atos run synthetic milestone\n")
                .await
                .map_err(|e| format!("add synthetic milestone: {e}"))?;
            m.id
        }
    };

    let run_ctx = orc
        .begin_run(&feature_id, &milestone_id, cfg.driver.label(), RunMode::Normal)
        .await
        .map_err(|e| format!("begin_run: {e}"))?;
    let run_id = run_ctx.run_id.clone();
    let run_dir = sovereign_runs_dir().join(&run_id);
    std::fs::create_dir_all(&run_dir)
        .map_err(|e| format!("create run dir {}: {e}", run_dir.display()))?;
    println!("atos run: feature={feature_id} run_id={run_id} dir={}", run_dir.display());

    let iter_log_path = run_dir.join("iterations.jsonl");

    // Operator override per ATOS_RUNNER.md § Stop conditions #3.
    // Short-circuit before any driver spawn: write a single iteration
    // record with verdict "operator_accept", close the run, exit.
    if cfg.accept {
        println!("atos run: ✓ operator override --accept; closing run without driver/reviewer");
        let now = chrono::Utc::now().to_rfc3339();
        let mut rec = IterationRecord::new(1, "OPERATOR_ACCEPT", &now);
        rec.verdict = "operator_accept".into();
        rec.ended_at = now.clone();
        append_jsonl(&iter_log_path, &rec)?;
        let _ = orc.close_run(&run_id, 0, true, None).await;
        return Ok(LoopOutcome::Accepted);
    }

    let plan_path = run_dir.join("plan.json");
    let workdir_plan_md = cfg.workdir.join("PLAN.md");
    // Workdir-resident "live" plan — the source of truth that
    // survives across `sovereign atos run` invocations. Each PLAN /
    // REASSESS save also writes here; runner startup loads from
    // here when --fresh-plan is not set, skipping the PLAN phase
    // entirely on resumption.
    let workdir_plan_path = cfg.workdir.join(".sovereign").join("plan.json");

    // Snapshot the workdir's git HEAD at run start so REASSESS /
    // EXECUTE can compute "diff since plan was written" cleanly. If
    // the workdir isn't a git repo, diffs degrade to empty.
    let plan_base_sha = git_rev_parse_head(&cfg.workdir).ok();

    // Plan resumption: prefer the workdir-resident plan when one
    // exists and the operator hasn't asked for a fresh start. This
    // makes `sovereign atos run` idempotent across invocations —
    // partial progress (steps already done) is preserved, and the
    // next run picks up at the next pending step instead of
    // re-planning from scratch.
    let mut plan: Option<Plan> = if cfg.fresh_plan {
        if workdir_plan_path.exists() {
            println!(
                "atos run: --fresh-plan set; ignoring existing {}",
                workdir_plan_path.display()
            );
        }
        None
    } else if let Ok(mut prior) = Plan::load(&workdir_plan_path) {
        // Any step left in `in_progress` state means the previous run
        // was killed mid-execution. Treat as failed so the FSM retries
        // them — otherwise `next_pending()` skips them and we resume
        // at a later step with missing prerequisites.
        for s in &mut prior.steps {
            if s.state == StepState::InProgress {
                s.state = StepState::Failed;
                s.last_failure = Some("previous run interrupted mid-execution".into());
            }
        }
        let done_count = prior
            .steps
            .iter()
            .filter(|s| matches!(s.state, StepState::Done | StepState::Skipped))
            .count();
        println!(
            "atos run: resumed plan from {} (rev {}, {}/{} steps already done)",
            workdir_plan_path.display(),
            prior.revision,
            done_count,
            prior.steps.len()
        );
        // Mirror into the run dir for audit; this run owns the live
        // plan from here on.
        let _ = prior.save(&plan_path);
        let _ = std::fs::write(&workdir_plan_md, render_plan_md(&prior));
        Some(prior)
    } else {
        Plan::load(&plan_path).ok()
    };
    let mut last_rejection: Option<RejectionMemo> = None;
    let mut accepted = false;
    let mut reviewer_failed = false;
    let mut stuck = false;
    let mut iter: u32 = 0;
    let mut steps_since_reassess: u32 = 0;
    let mut just_failed_step = false;
    // Cadence reassess every K steps. K=5 lets a meaningful streak
    // of EXECUTEs accumulate before pausing for plan review; K=3 was
    // too aggressive — it kept interrupting working runs to ask the
    // agent to regenerate a plan it couldn't reliably regenerate.
    let reassess_every_k: u32 = 5;
    let mut consecutive_reassess_failures: u32 = 0;
    const MAX_CONSECUTIVE_REASSESS_FAILURES: u32 = 2;

    while iter < cfg.max_iters {
        iter += 1;
        let phase = decide_phase(
            plan.as_ref(),
            last_rejection.as_ref(),
            steps_since_reassess,
            reassess_every_k,
            just_failed_step,
        );
        just_failed_step = false;

        let phase_label = match &phase {
            Phase::Plan => "plan".to_string(),
            Phase::Execute(id) => format!("execute({id})"),
            Phase::Reassess(t) => format!("reassess({:?})", t),
            Phase::Final => "final".to_string(),
            Phase::Stuck => "stuck".to_string(),
        };
        let iter_start = Instant::now();
        let started_at = chrono::Utc::now().to_rfc3339();
        let iter_dir = run_dir.join(format!("iter-{iter:03}-{}", phase_label.replace(['(', ')', ',', ' '], "_")));
        std::fs::create_dir_all(&iter_dir)
            .map_err(|e| format!("create iter dir {}: {e}", iter_dir.display()))?;

        println!(
            "\natos run: ─── iter {iter:03} · phase={phase_label} ──────────────────────"
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();

        let mut record = IterationRecord::new(iter, &phase_label, &started_at);

        match phase {
            Phase::Stuck => {
                eprintln!(
                    "atos run: STUCK — a step failed twice and reassess couldn't unstick it. \
                     Inspect runs/<run-id>/ for details and consider editing plan.json by hand."
                );
                record.verdict = "stuck".into();
                record.wall_seconds = iter_start.elapsed().as_secs();
                record.ended_at = chrono::Utc::now().to_rfc3339();
                append_jsonl(&iter_log_path, &record)?;
                stuck = true;
                break;
            }
            Phase::Plan => {
                let prompt = build_plan_prompt(
                    &artifacts,
                    &feature_id,
                    &cfg.workdir,
                    last_rejection.as_ref(),
                );
                let prompt_path = iter_dir.join("prompt.md");
                std::fs::write(&prompt_path, &prompt)
                    .map_err(|e| format!("write {}: {e}", prompt_path.display()))?;
                record.prompt_sha = sha256_hex(prompt.as_bytes());

                if cfg.dry_run {
                    println!("atos run: [dry-run] would invoke PLAN agent");
                    record.verdict = "dry_run".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                let exit = run_driver(&cfg, &prompt, &feature_id, &run_id);
                record.opencode_exit = exit;
                if exit == 130 {
                    eprintln!("atos run: driver exited via SIGINT — stopping loop");
                    record.verdict = "operator_interrupt".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                // The agent's atos_plan_emit tool wrote plan.json to
                // <workdir>/.sovereign/plan.json directly — read it
                // back here. If the tool wasn't called (agent emitted
                // prose instead, etc.), fall back to the legacy
                // text-extract path so we tolerate older agent
                // behaviour.
                // Source-of-truth pivot: agent edits <workdir>/PLAN.md
                // via its standard `write` tool. Parse markdown back
                // into a Plan. Legacy paths (json on disk, prose JSON
                // extract) survive as fallbacks so partial migrations
                // and resumed runs keep working.
                let load_result: Result<Plan, String> = match std::fs::read_to_string(
                    &workdir_plan_md,
                ) {
                    Ok(text) => match parse_plan_md(&text) {
                        Ok(mut parsed) => {
                            if parsed.feature_id.trim().is_empty() {
                                parsed.feature_id = feature_id.clone();
                            }
                            if parsed.created_at.trim().is_empty() {
                                parsed.created_at = chrono::Utc::now().to_rfc3339();
                            }
                            parsed.validate().map(|_| parsed)
                        }
                        Err(e) => Err(e),
                    },
                    Err(_) => {
                        if workdir_plan_path.exists() {
                            Plan::load(&workdir_plan_path)
                        } else {
                            // PLAN.md not on disk — the agent may have
                            // emitted it via a `write` tool call (pure
                            // tool-call responses don't show up in text
                            // extraction). Try extracting from the
                            // opencode session text first; fall back to
                            // fenced-JSON extraction.
                            let text = fetch_last_assistant_text(&cfg.workdir);
                            let defaults = PlanDefaults {
                                feature_id: feature_id.clone(),
                                design_sha: None,
                                created_at: chrono::Utc::now().to_rfc3339(),
                            };
                            match try_parse_tool_emitted_plan(&text, &defaults, true) {
                                Some(result) => result,
                                None => extract_and_validate_plan(&cfg.workdir, &iter_dir),
                            }
                        }
                    }
                };
                match load_result {
                    Ok(p) => {
                        // Validate plan against workdir state.
                        // A plan whose first step runs `cargo test`
                        // when Cargo.toml doesn't exist is structurally
                        // valid but impossible to execute — the agent
                        // skipped the scaffold step entirely.
                        let step_verify_cmds: Vec<String> = p.steps.iter().map(|s| s.verify_cmd.clone()).collect();
                        let step01_files: Vec<String> = p.steps.first().map(|s| s.files_touched.clone()).unwrap_or_default();
                        if let Some(gap) = detect_missing_scaffold(&step_verify_cmds, &step01_files, &cfg.workdir) {
                            record.verdict = "plan_invalid".into();
                            record.notes = Some(gap);
                            record.wall_seconds = iter_start.elapsed().as_secs();
                            record.ended_at = chrono::Utc::now().to_rfc3339();
                            append_jsonl(&iter_log_path, &record)?;
                            continue;
                        }
                        save_plan_dual(&p, &plan_path, &workdir_plan_path, &workdir_plan_md)?;
                        println!(
                            "atos run: ✓ plan written ({} steps, revision {})",
                            p.steps.len(),
                            p.revision
                        );
                        record.verdict = "plan_written".into();
                        record.notes = Some(format!(
                            "{} steps, revision {}",
                            p.steps.len(),
                            p.revision
                        ));
                        plan = Some(p);
                        steps_since_reassess = 0;
                        // Clear the carried-over rejection memo from
                        // any prior plan_invalid iteration. Without
                        // this, `decide_phase` keeps routing to
                        // Reassess(ReviewerReject) after a successful
                        // plan emit — the agent never gets to Execute
                        // because the FSM still thinks "the reviewer
                        // rejected your last plan" (it didn't; the
                        // PARSER did, two iters ago, and we just fixed
                        // it). Observed on 2026-05-12 codex smoke:
                        // iter 1 plan_invalid → iter 2 plan_written →
                        // iter 3 reassess(ReviewerReject) loop instead
                        // of execute. Max-iters=3 ran out before any
                        // step ever ran.
                        last_rejection = None;
                    }
                    Err(e) => {
                        eprintln!("atos run: plan agent did not produce a valid plan ({e})");
                        record.verdict = "plan_invalid".into();
                        record.notes = Some(e.clone());
                        // Surface the structural reason to the next
                        // plan-phase prompt. Without this the next
                        // iteration sends the exact same prompt and
                        // the agent makes the exact same mistake.
                        //
                        // Cap the prior-attempt paste at 4 KB so we
                        // don't run away with the plan-phase prompt
                        // when an agent emits a giant malformed dump.
                        // Models pattern-match shape from the first
                        // dozen lines; the cap protects context-window
                        // budget without losing signal.
                        let prior_text = std::fs::read_to_string(&workdir_plan_md)
                            .ok()
                            .map(|s| truncate(&s, 4096));
                        let prior_count = last_rejection
                            .as_ref()
                            .map(|m| m.attempt_count)
                            .unwrap_or(0);
                        last_rejection = Some(RejectionMemo {
                            summary: format!(
                                "Your last PLAN.md was rejected by the runner's parser. Error: {e}"
                            ),
                            gaps: vec![Gap {
                                area: "PLAN.md structure".into(),
                                what_missing: e,
                                suggested_action: "Rewrite PLAN.md from scratch using exactly the `## step-NN: <goal> [PENDING]` heading shape shown in the Required structure section. Do NOT preserve the operator's draft numbered-list format — restructure entirely.".into(),
                            }],
                            prior_attempt_text: prior_text,
                            attempt_count: prior_count + 1,
                        });
                    }
                }
                record.wall_seconds = iter_start.elapsed().as_secs();
                record.ended_at = chrono::Utc::now().to_rfc3339();
                append_jsonl(&iter_log_path, &record)?;
            }
            Phase::Execute(step_id) => {
                let mut p = plan.clone().expect("plan must exist for Execute");
                let step_idx = p
                    .steps
                    .iter()
                    .position(|s| s.id == step_id)
                    .ok_or_else(|| format!("step {step_id} vanished from plan"))?;
                let diff = match plan_base_sha.as_deref() {
                    Some(base) => git_diff_against(&cfg.workdir, base).unwrap_or_default(),
                    None => String::new(),
                };
                let recent_notes = String::new(); // future: pull from NoteStore
                let atos_context = refresh_atos_context(&cfg.workdir, Some(&p));
                // Persist context so the operator can read it too.
                if let Some(parent) = context_md_path(&cfg.workdir).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(context_md_path(&cfg.workdir), &atos_context);
                let prompt = build_execute_prompt(
                    &p.steps[step_idx],
                    &p,
                    &cfg.workdir,
                    &diff,
                    &recent_notes,
                    &atos_context,
                    artifacts.design.as_ref().map(|d| d.body.as_str()),
                );
                let prompt_path = iter_dir.join("prompt.md");
                std::fs::write(&prompt_path, &prompt)
                    .map_err(|e| format!("write {}: {e}", prompt_path.display()))?;
                record.prompt_sha = sha256_hex(prompt.as_bytes());
                record.step_id = Some(step_id.clone());

                if cfg.dry_run {
                    println!(
                        "atos run: [dry-run] would execute {} (verify={})",
                        step_id, p.steps[step_idx].verify_cmd
                    );
                    record.verdict = "dry_run".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                p.steps[step_idx].state = StepState::InProgress;
                p.steps[step_idx].attempts += 1;
                save_plan_dual(&p, &plan_path, &workdir_plan_path, &workdir_plan_md)?;

                // (Removed 2026-05-06: Rust-specific pre-seeding of
                // `Cargo.toml`. It overwrote the agent's intended
                // crate name with a slugified workdir basename and
                // leaked Rust assumptions into the language-agnostic
                // tooling layer. The FSM's normal failure→REASSESS
                // loop, plus the verify_cmd gates and parse-aware
                // pattern_fix_guidance, recover from bad manifests
                // without forcing a stub on the agent. The
                // `pre_seed_cargo_toml_if_needed` function survives
                // for tests / emergency reuse but is unwired from
                // EXECUTE.)

                // Snapshot `files_touched` mtimes immediately
                // before the driver runs. After it exits we use
                // these to detect the silent-no-op failure mode:
                // agent claims completion, verify_cmd passes
                // (because the prior on-disk state already
                // compiles), but no listed file was actually
                // modified. See `detect_untouched_files`.
                let pre_run_mtimes =
                    snapshot_file_mtimes(&cfg.workdir, &p.steps[step_idx].files_touched);

                let exit = run_driver(&cfg, &prompt, &feature_id, &run_id);
                record.opencode_exit = exit;
                if exit == 130 {
                    eprintln!("atos run: driver exited via SIGINT — stopping loop");
                    p.steps[step_idx].state = StepState::Failed;
                    p.steps[step_idx].last_failure = Some("operator interrupt".into());
                    save_plan_dual(&p, &plan_path, &workdir_plan_path, &workdir_plan_md)?;
                    record.verdict = "operator_interrupt".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                let (verify_passed, verify_log) =
                    run_verify_cmd(&cfg.workdir, &p.steps[step_idx].verify_cmd).await;
                let verify_log_path = iter_dir.join("verify.log");
                let _ = std::fs::write(&verify_log_path, &verify_log);
                p.steps[step_idx].last_verify_stdout = Some(truncate(&verify_log, 4 * 1024));

                // Hollow-file gate: a passing verify_cmd doesn't
                // count if the step's declared `files_touched` are
                // empty (or were never created). Catches the
                // "scaffolded an empty src/lib.rs and cargo check
                // happily compiled it" failure mode the FSM otherwise
                // misreads as success.
                let hollow_warning =
                    detect_hollow_files(&cfg.workdir, &p.steps[step_idx].files_touched);
                // Untouched-files gate: a passing verify_cmd also
                // doesn't count if the agent didn't actually write
                // any of the files declared in `files_touched`.
                // Complements the hollow gate — hollow catches
                // "file is empty after"; untouched catches "file
                // wasn't modified during this iter at all" (e.g.
                // the file was already substantive before EXECUTE
                // ran, so the byte-count threshold passes
                // misleadingly).
                let untouched_warning = detect_untouched_files(
                    &cfg.workdir,
                    &p.steps[step_idx].files_touched,
                    &pre_run_mtimes,
                );
                let effective_pass =
                    verify_passed && hollow_warning.is_none() && untouched_warning.is_none();

                if effective_pass {
                    println!(
                        "atos run: ✓ {} verified ({})",
                        step_id, p.steps[step_idx].verify_cmd
                    );
                    p.steps[step_idx].state = StepState::Done;
                    p.steps[step_idx].last_failure = None;
                    record.verdict = "step_done".into();
                    record.verify_passed = Some(true);
                    steps_since_reassess += 1;
                    // A successful step proves forward motion — clear
                    // the consecutive-reassess-failure counter so the
                    // Stuck cap doesn't fire spuriously when reassess
                    // failures are spread across many iters with
                    // healthy progress between them.
                    consecutive_reassess_failures = 0;

                    // Cargo.toml exists now (or earlier). Rewrite any
                    // remaining-step verify_cmds whose `-p X` argument
                    // doesn't match the canonical crate name. Catches
                    // the classic PLAN-time hallucinated-typo failure
                    // mode without a model round-trip.
                    if let Ok(cargo_body) =
                        std::fs::read_to_string(cfg.workdir.join("Cargo.toml"))
                    {
                        if let Some(canonical) = parse_crate_name(&cargo_body) {
                            if canonicalize_verify_cmds(&mut p, &canonical) {
                                println!(
                                    "atos run: canonicalized verify_cmds against `{}`",
                                    canonical
                                );
                            }
                        }
                    }
                } else {
                    // Compose a tight failure reason. Order matters:
                    // when verify itself failed, that's the real story
                    // — lead with the verify_log, and only mention
                    // gate warnings as follow-up. The previous ordering
                    // (hollow/untouched first) produced misleading
                    // "verify_cmd exited 0" messaging when verify
                    // actually crashed, gaslighting the agent on retry.
                    let reason = if !verify_passed {
                        let mut msg = verify_log.clone();
                        if let Some(h) = hollow_warning {
                            msg.push_str(&format!("\n\n(also: {h})"));
                        }
                        if let Some(u) = untouched_warning {
                            msg.push_str(&format!("\n\n(also: {u})"));
                        }
                        msg
                    } else if let Some(h) = hollow_warning {
                        println!(
                            "atos run: ✗ {} verify exited 0 but files are hollow — {}",
                            step_id, h
                        );
                        format!(
                            "verify_cmd exited 0 but hollow-file gate failed: {h}\n\nverify output:\n{verify_log}"
                        )
                    } else if let Some(u) = untouched_warning {
                        println!(
                            "atos run: ✗ {} verify exited 0 but agent didn't touch any declared files — {}",
                            step_id, u
                        );
                        format!(
                            "verify_cmd exited 0 but untouched-files gate failed: {u}\n\nverify output:\n{verify_log}"
                        )
                    } else {
                        println!(
                            "atos run: ✗ {} verify failed (attempt {})",
                            step_id, p.steps[step_idx].attempts
                        );
                        verify_log
                    };
                    p.steps[step_idx].state = StepState::Failed;
                    p.steps[step_idx].last_failure = Some(reason);
                    record.verdict = "step_failed".into();
                    record.verify_passed = Some(false);
                    just_failed_step = true;
                }
                save_plan_dual(&p, &plan_path, &workdir_plan_path, &workdir_plan_md)?;
                plan = Some(p);
                record.wall_seconds = iter_start.elapsed().as_secs();
                record.ended_at = chrono::Utc::now().to_rfc3339();
                append_jsonl(&iter_log_path, &record)?;
            }
            Phase::Reassess(trigger) => {
                let p = plan.clone().expect("plan must exist for Reassess");
                let diff = match plan_base_sha.as_deref() {
                    Some(base) => git_diff_against(&cfg.workdir, base).unwrap_or_default(),
                    None => String::new(),
                };
                let decision_summary = String::new(); // future: pull from NoteStore
                let atos_context = refresh_atos_context(&cfg.workdir, Some(&p));
                if let Some(parent) = context_md_path(&cfg.workdir).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(context_md_path(&cfg.workdir), &atos_context);
                let prompt = build_reassess_prompt(
                    &p,
                    &trigger,
                    &diff,
                    &decision_summary,
                    last_rejection.as_ref(),
                    &atos_context,
                    &cfg.workdir,
                );
                let prompt_path = iter_dir.join("prompt.md");
                std::fs::write(&prompt_path, &prompt)
                    .map_err(|e| format!("write {}: {e}", prompt_path.display()))?;
                record.prompt_sha = sha256_hex(prompt.as_bytes());

                if cfg.dry_run {
                    record.verdict = "dry_run".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                let exit = run_driver(&cfg, &prompt, &feature_id, &run_id);
                record.opencode_exit = exit;
                if exit == 130 {
                    record.verdict = "operator_interrupt".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                // For REASSESS, the agent rewrites PLAN.md with a
                // bumped revision. Read the file back, parse, merge
                // execution state from the in-memory prior plan.
                // Fall back to legacy paths when PLAN.md is absent.
                let prior_feature_id = p.feature_id.clone();
                let prior_design_sha = p.design_sha.clone();
                let prior_created_at = p.created_at.clone();
                let prior_revision = p.revision;
                let plan_md_path = cfg.workdir.join("PLAN.md");
                let reassess_defaults = PlanDefaults {
                    feature_id: prior_feature_id.clone(),
                    design_sha: Some(prior_design_sha.clone()),
                    created_at: prior_created_at.clone(),
                };
                let load_result: Result<Plan, String> = match std::fs::read_to_string(
                    &plan_md_path,
                ) {
                    Ok(text) => parse_plan_md(&text).map(|mut parsed| {
                        apply_plan_defaults(&mut parsed, &reassess_defaults);
                        parsed
                    }),
                    Err(_) => {
                        // Try tool-call extraction first (same logic as
                        // PLAN phase — the agent may have emitted the
                        // new plan purely via a write tool call).
                        let text = fetch_last_assistant_text(&cfg.workdir);
                        match try_parse_tool_emitted_plan(&text, &reassess_defaults, false) {
                            Some(result) => result,
                            None => match Plan::load(&workdir_plan_path) {
                                Ok(disk) if disk.revision > prior_revision => Ok(disk),
                                _ => extract_and_validate_plan_with_defaults(
                                    &cfg.workdir,
                                    &iter_dir,
                                    &prior_feature_id,
                                    &prior_design_sha,
                                    &prior_created_at,
                                ),
                            },
                        }
                    },
                };
                match load_result {
                    Ok(mut new_plan) => {
                        if new_plan.revision <= p.revision {
                            // Auto-bump if the agent forgot — minor
                            // robustness over rejecting a useful plan.
                            new_plan.revision = p.revision + 1;
                        }
                        // Carry over execution state AND structural
                        // metadata from the previous plan. REASSESS
                        // agents often emit steps with just the field
                        // they want to change (`verify_cmd`) and omit
                        // `goal` / `files_touched` / `rationale`. We
                        // merge the omissions back from the prior
                        // plan so the run keeps moving instead of
                        // failing validation. Steps added by REASSESS
                        // that don't appear in the prior plan must
                        // supply their own `goal` and `verify_cmd`.
                        let by_id: std::collections::HashMap<&str, &Step> =
                            p.steps.iter().map(|s| (s.id.as_str(), s)).collect();
                        for s in new_plan.steps.iter_mut() {
                            if let Some(prev) = by_id.get(s.id.as_str()) {
                                if prev.state == StepState::Done {
                                    s.state = StepState::Done;
                                }
                                if matches!(prev.state, StepState::Failed) && s.state == StepState::Pending {
                                    s.attempts = prev.attempts;
                                    s.last_failure = prev.last_failure.clone();
                                    s.last_verify_stdout = prev.last_verify_stdout.clone();
                                }
                                // Fill structural fields the agent
                                // omitted, but DON'T overwrite ones
                                // they explicitly emitted. This lets
                                // REASSESS rewrite `verify_cmd` while
                                // leaving `goal` intact, or rewrite
                                // `goal` while keeping `verify_cmd`.
                                if s.goal.trim().is_empty() {
                                    s.goal = prev.goal.clone();
                                }
                                if s.verify_cmd.trim().is_empty() {
                                    s.verify_cmd = prev.verify_cmd.clone();
                                }
                                if s.files_touched.is_empty() {
                                    s.files_touched = prev.files_touched.clone();
                                }
                                if s.rationale.trim().is_empty() {
                                    s.rationale = prev.rationale.clone();
                                }
                            }
                        }
                        // Re-validate after the merge — covers the
                        // edge case where REASSESS adds a brand-new
                        // step missing `goal`/`verify_cmd` that the
                        // merge couldn't fill in. Failed validation
                        // here keeps the prior plan; we surface the
                        // error through the iteration log.
                        if let Err(e) = new_plan.validate() {
                            eprintln!("atos run: post-merge validation failed: {e}");
                            record.verdict = "reassess_invalid".into();
                            record.notes = Some(format!("post-merge: {e}"));
                            consecutive_reassess_failures += 1;
                            record.wall_seconds = iter_start.elapsed().as_secs();
                            record.ended_at = chrono::Utc::now().to_rfc3339();
                            append_jsonl(&iter_log_path, &record)?;
                            continue;
                        }
                        save_plan_dual(&new_plan, &plan_path, &workdir_plan_path, &workdir_plan_md)?;
                        println!(
                            "atos run: ✓ plan reassessed → revision {}",
                            new_plan.revision
                        );
                        record.verdict = "reassessed".into();
                        record.notes = Some(format!("revision {}", new_plan.revision));
                        plan = Some(new_plan);
                        steps_since_reassess = 0;
                        last_rejection = None;
                        consecutive_reassess_failures = 0;
                    }
                    Err(e) => {
                        eprintln!("atos run: reassess produced invalid plan ({e}); keeping prior");
                        record.verdict = "reassess_invalid".into();
                        record.notes = Some(e);
                        // Cadence-triggered reassesses are
                        // best-effort. If the model couldn't emit
                        // a valid update, that's fine — reset
                        // `steps_since_reassess` so the FSM doesn't
                        // immediately bounce back into Reassess on
                        // the next iter. Step-failure-triggered
                        // reassesses still feed the consecutive-
                        // failure counter; only failures during
                        // active blockage should trip the Stuck cap.
                        if matches!(trigger, ReassessTrigger::Cadence) {
                            steps_since_reassess = 0;
                            // Don't count cadence failures toward Stuck.
                        } else {
                            consecutive_reassess_failures += 1;
                            if consecutive_reassess_failures >= MAX_CONSECUTIVE_REASSESS_FAILURES {
                                eprintln!(
                                    "atos run: STUCK — reassess failed {} times in a row while a step was blocked",
                                    consecutive_reassess_failures
                                );
                                stuck = true;
                                record.wall_seconds = iter_start.elapsed().as_secs();
                                record.ended_at = chrono::Utc::now().to_rfc3339();
                                append_jsonl(&iter_log_path, &record)?;
                                break;
                            }
                        }
                    }
                }
                record.wall_seconds = iter_start.elapsed().as_secs();
                record.ended_at = chrono::Utc::now().to_rfc3339();
                append_jsonl(&iter_log_path, &record)?;
            }
            Phase::Final => {
                let prompt =
                    compose_agent_prompt(iter, &artifacts, last_rejection.as_ref());
                let prompt_path = iter_dir.join("prompt.md");
                std::fs::write(&prompt_path, &prompt)
                    .map_err(|e| format!("write {}: {e}", prompt_path.display()))?;
                record.prompt_sha = sha256_hex(prompt.as_bytes());

                if cfg.dry_run {
                    record.verdict = "dry_run".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                let exit = run_driver(&cfg, &prompt, &feature_id, &run_id);
                record.opencode_exit = exit;
                if exit == 130 {
                    record.verdict = "operator_interrupt".into();
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    break;
                }

                let done_path = cfg.workdir.join(&cfg.done_marker);
                let done_present = done_path.is_file();
                record.done_present = Some(done_present);
                if !done_present {
                    record.verdict = "no_done".into();
                    let prior_count = last_rejection
                        .as_ref()
                        .map(|m| m.attempt_count)
                        .unwrap_or(0);
                    last_rejection = Some(RejectionMemo {
                        summary: format!(
                            "FINAL phase ended without {} written. Reassess and retry.",
                            cfg.done_marker
                        ),
                        gaps: vec![Gap {
                            area: "completion claim".into(),
                            what_missing: format!("{} not written", cfg.done_marker),
                            suggested_action: format!("Write {}.", cfg.done_marker),
                        }],
                        prior_attempt_text: None,
                        attempt_count: prior_count + 1,
                    });
                    record.wall_seconds = iter_start.elapsed().as_secs();
                    record.ended_at = chrono::Utc::now().to_rfc3339();
                    append_jsonl(&iter_log_path, &record)?;
                    continue;
                }

                // ATOS_RUNNER.md § Stop conditions: charter-defined
                // hard gate runs BEFORE the reviewer. Exit non-zero
                // → soft-reject and continue; exit zero → proceed to
                // reviewer as the second-layer judge.
                if let Some(cmd) = stop_condition.as_deref() {
                    let outcome = run_stop_condition(&cfg.workdir, cmd);
                    let _ = std::fs::write(
                        iter_dir.join("stop_condition.log"),
                        format!(
                            "$ {cmd}\nexit: {}\n--- stdout ---\n{}\n--- stderr ---\n{}\n",
                            outcome.exit_code, outcome.stdout, outcome.stderr
                        ),
                    );
                    if outcome.exit_code != 0 {
                        println!(
                            "atos run: ✗ stop_condition `{cmd}` exited {} — soft-rejecting",
                            outcome.exit_code
                        );
                        record.verdict = "stop_condition_failed".into();
                        let archived = iter_dir.join("DONE.rejected.md");
                        let _ = std::fs::rename(&done_path, &archived);
                        let tail = truncate(&outcome.stderr, 2000);
                        let stdout_tail = truncate(&outcome.stdout, 2000);
                        let prior_count = last_rejection
                            .as_ref()
                            .map(|m| m.attempt_count)
                            .unwrap_or(0);
                        last_rejection = Some(RejectionMemo {
                            summary: format!(
                                "Charter stop_condition `{cmd}` exited {}. Reviewer was not consulted. Fix the failing check and rewrite DONE.md.",
                                outcome.exit_code
                            ),
                            gaps: vec![Gap {
                                area: "stop_condition".into(),
                                what_missing: format!(
                                    "`{cmd}` did not exit zero (got {})",
                                    outcome.exit_code
                                ),
                                suggested_action: format!(
                                    "Investigate the failure and re-run. Last stderr (truncated): {tail}\nLast stdout (truncated): {stdout_tail}"
                                ),
                            }],
                            prior_attempt_text: None,
                            attempt_count: prior_count + 1,
                        });
                        record.wall_seconds = iter_start.elapsed().as_secs();
                        record.ended_at = chrono::Utc::now().to_rfc3339();
                        append_jsonl(&iter_log_path, &record)?;
                        continue;
                    }
                    println!("atos run: ✓ stop_condition `{cmd}` passed");
                }

                let done_md = std::fs::read_to_string(&done_path)
                    .map_err(|e| format!("read {}: {e}", done_path.display()))?;
                let diff_md = match plan_base_sha.as_deref() {
                    Some(base) => git_diff_against(&cfg.workdir, base).unwrap_or_default(),
                    None => String::new(),
                };
                let reviewer_in = ReviewerInputs {
                    artifacts: &artifacts,
                    done_md: &done_md,
                    diff_md: &diff_md,
                    workdir: &cfg.workdir,
                    iter,
                };
                let verdict = match call_reviewer(&cfg, &reviewer_in).await {
                    Ok(v) => v,
                    Err(e) => {
                        record.verdict = "reviewer_error".into();
                        record.reviewer_error = Some(e.to_string());
                        record.wall_seconds = iter_start.elapsed().as_secs();
                        record.ended_at = chrono::Utc::now().to_rfc3339();
                        append_jsonl(&iter_log_path, &record)?;
                        reviewer_failed = true;
                        break;
                    }
                };
                std::fs::write(
                    iter_dir.join("verdict.json"),
                    serde_json::to_string_pretty(&verdict).unwrap_or_default(),
                )
                .ok();
                record.gap_count = Some(verdict.gaps.len() as u32);
                match verdict.verdict.as_str() {
                    "accept" => {
                        println!("atos run: ✓ reviewer ACCEPTED");
                        record.verdict = "accept".into();
                        accepted = true;
                        record.wall_seconds = iter_start.elapsed().as_secs();
                        record.ended_at = chrono::Utc::now().to_rfc3339();
                        append_jsonl(&iter_log_path, &record)?;
                        break;
                    }
                    "reject" => {
                        println!(
                            "atos run: ✗ reviewer REJECTED — {} gap(s); will reassess",
                            verdict.gaps.len()
                        );
                        let archived = iter_dir.join("DONE.rejected.md");
                        let _ = std::fs::rename(&done_path, &archived);
                        record.verdict = "reject".into();
                        // Snapshot DONE.md the agent just wrote so the
                        // next prompt can show it back and pattern-
                        // match the rejection — same idea as the
                        // plan-phase prior_attempt_text but for the
                        // execute artifact. Read from the archived
                        // copy since we just renamed it.
                        let prior_text = std::fs::read_to_string(&archived)
                            .ok()
                            .map(|s| truncate(&s, 4096));
                        let prior_count = last_rejection
                            .as_ref()
                            .map(|m| m.attempt_count)
                            .unwrap_or(0);
                        last_rejection = Some(RejectionMemo {
                            summary: verdict.summary.clone(),
                            gaps: verdict.gaps.clone(),
                            prior_attempt_text: prior_text,
                            attempt_count: prior_count + 1,
                        });
                    }
                    other => {
                        record.verdict = format!("unknown:{other}");
                        let prior_count = last_rejection
                            .as_ref()
                            .map(|m| m.attempt_count)
                            .unwrap_or(0);
                        last_rejection = Some(RejectionMemo {
                            summary: format!("Reviewer returned '{other}'; reassessing"),
                            gaps: verdict.gaps.clone(),
                            prior_attempt_text: None,
                            attempt_count: prior_count + 1,
                        });
                    }
                }
                record.wall_seconds = iter_start.elapsed().as_secs();
                record.ended_at = chrono::Utc::now().to_rfc3339();
                append_jsonl(&iter_log_path, &record)?;
            }
        }
    }

    let _ = orc
        .close_run(&run_id, if accepted { 0 } else { 1 }, accepted, None)
        .await;

    if accepted {
        Ok(LoopOutcome::Accepted)
    } else if reviewer_failed {
        Ok(LoopOutcome::ReviewerFailure)
    } else if stuck {
        Ok(LoopOutcome::Stuck)
    } else {
        eprintln!(
            "atos run: exhausted --max-iters {} without acceptance",
            cfg.max_iters
        );
        Ok(LoopOutcome::ExhaustedIterations)
    }
}

/// Wrapper around `spawn_driver` that consolidates the flush-then-spawn
/// pattern + error logging the FSM phases all use.
fn run_driver(cfg: &RunCfg, prompt: &str, feature_id: &str, run_id: &str) -> i32 {
    println!(
        "atos run: spawning {} (model={}) in {}…",
        cfg.driver.label(),
        cfg.driver_model,
        cfg.workdir.display()
    );
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    match spawn_driver(
        cfg.driver,
        &cfg.driver_model,
        &cfg.workdir,
        prompt,
        feature_id,
        run_id,
    ) {
        Ok(s) => s.code().unwrap_or(-1),
        Err(e) => {
            eprintln!("atos run: driver spawn failed: {e}");
            -1
        }
    }
}

/// Open the orchestrator's stores rooted at `workdir/.sovereign/` so
/// audit anchors (features.db, notes.db) live next to the project the
/// runner is driving — not next to wherever the operator typed the
/// command. The directory is created if missing so a fresh project can
/// be driven immediately after `sovereign init` (or even before — the
/// runner is permissive about provisioning when the charter shape
/// doesn't conform).
fn open_orchestrator_for(workdir: &Path) -> Result<Arc<LocalAtosOrchestrator>, String> {
    let sovereign_dir = workdir.join(".sovereign");
    std::fs::create_dir_all(&sovereign_dir)
        .map_err(|e| format!("create {}: {e}", sovereign_dir.display()))?;
    let features = FeatureStore::open(&sovereign_dir.join("features.db"))
        .map(Arc::new)
        .map_err(|e| format!("open features.db: {e}"))?;
    let notes = NoteStore::open(&sovereign_dir.join("notes.db"))
        .map(Arc::new)
        .map_err(|e| format!("open notes.db: {e}"))?;
    let mut orc = LocalAtosOrchestrator::new(features, notes);
    if let Ok(store) =
        corpus_engine::ProjectDocsStore::open(&sovereign_dir.join("project_docs.db"))
    {
        orc = orc.with_project_docs(Arc::new(store), workdir.to_path_buf());
    }
    Ok(Arc::new(orc))
}

async fn resolve_or_provision_feature(
    orc: &LocalAtosOrchestrator,
    cfg: &RunCfg,
    artifacts: &ResolvedArtifacts,
) -> Result<String, String> {
    if let Some(id) = cfg.feature_id.as_ref() {
        if orc
            .get_feature(id)
            .await
            .map_err(|e| format!("get feature {id}: {e}"))?
            .is_some()
        {
            return Ok(id.clone());
        }
    }

    // Attempt to provision from the charter (the structured shape the
    // existing orchestrator understands).
    if let Some(charter) = artifacts.charter.as_ref() {
        match orc.provision_feature(&charter.body).await {
            Ok(row) => return Ok(row.id),
            Err(sovereign_atos::Error::CharterParse(_)) => {
                // Charter doesn't conform to the structured-milestone
                // shape; fall through to the synthetic-feature path.
            }
            Err(e) => return Err(format!("provision_feature: {e}")),
        }
    }

    // Synthesize a feature from the workdir basename so we always have
    // an audit anchor. The id is stable across reruns of the same
    // workdir.
    let id = cfg
        .feature_id
        .clone()
        .unwrap_or_else(|| {
            cfg.workdir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "atos-run".into())
        });
    if orc
        .get_feature(&id)
        .await
        .map_err(|e| format!("get feature {id}: {e}"))?
        .is_some()
    {
        return Ok(id);
    }
    let title = format!("atos run: {}", id);
    let charter_md = artifacts
        .charter
        .as_ref()
        .map(|c| c.body.clone())
        .unwrap_or_else(|| {
            "(no charter — runner provisioned from workdir basename)\n".to_string()
        });
    orc.provision_feature_parts(&id, &title, &charter_md, "", "")
        .await
        .map_err(|e| format!("provision synthetic feature {id}: {e}"))?;
    Ok(id)
}

// ─── Prompt composition ──────────────────────────────────────────────────────

fn compose_agent_prompt(
    iter: u32,
    artifacts: &ResolvedArtifacts,
    last_rejection: Option<&RejectionMemo>,
) -> String {
    let mut out = String::new();
    out.push_str("# ATOS run — agent session brief\n\n");
    out.push_str(&format!("**Iteration:** {iter}\n\n"));
    out.push_str(
        "You are working under the ATOS Runner. The runner will keep \
         re-spawning you until you produce a `DONE.md` at the workdir \
         root that a reviewer accepts against the project's charter and \
         design.\n\n",
    );
    out.push_str("## DONE contract\n\n");
    out.push_str(
        "When you believe the work meets the design (and, if a plan is \
         present, satisfies its phases), write `DONE.md` at the workdir \
         root. Structure it as:\n\
         \n\
         1. One section per design anchor or plan phase, citing the code \
            that satisfies it (`path:line` references).\n\
         2. A final `## What I did NOT do` section listing anything you \
            skipped, punted on, or left as TODO. Be honest — the reviewer \
            uses this section to decide whether the gap is acceptable for \
            this charter.\n\
         \n\
         A reviewer will then read DONE against the charter. If a \
         requirement is missing or a punt is unacceptable, the reviewer \
         will reject and you'll see specific feedback in the next \
         iteration.\n\n",
    );

    if let Some(charter) = artifacts.charter.as_ref() {
        out.push_str(&format!("## Charter ({})\n\n", charter.label));
        out.push_str(charter.body.trim());
        out.push_str("\n\n");
    }
    if let Some(design) = artifacts.design.as_ref() {
        out.push_str(&format!("## Design ({})\n\n", design.label));
        out.push_str(design.body.trim());
        out.push_str("\n\n");
    }
    if let Some(plan) = artifacts.plan.as_ref() {
        out.push_str(&format!("## Implementation plan ({})\n\n", plan.label));
        out.push_str(plan.body.trim());
        out.push_str("\n\n");
    }

    if let Some(rej) = last_rejection {
        out.push_str("## Reviewer feedback from previous iteration\n\n");
        out.push_str(&rej.summary);
        out.push_str("\n\n");
        if !rej.gaps.is_empty() {
            out.push_str("### Specific gaps to close\n\n");
            for (i, g) in rej.gaps.iter().enumerate() {
                out.push_str(&format!(
                    "{}. **{}** — {}\n   *Suggested:* {}\n",
                    i + 1,
                    g.area,
                    g.what_missing,
                    g.suggested_action
                ));
            }
            out.push('\n');
        }
        out.push_str(
            "Treat the gaps above as the iteration goal. The reviewer \
             will look for them specifically when grading the next DONE.\n\n",
        );
    }

    if iter == 1 {
        out.push_str(
            "## Starting fresh\n\n\
             Take stock of the workdir, then begin. Use the tools available \
             to you (symbols, code_search, callers, callees) instead of \
             reading whole files when looking up specific things. Write \
             notes (`note` / `decision` / `invariant`) at the moment of \
             each non-trivial choice — those become the audit trail this \
             charter is graded against.\n\n",
        );
    } else {
        out.push_str(
            "## Continuing from current state\n\n\
             The repo is where the previous iteration left it. Don't \
             reset; build on top.\n\n",
        );
    }

    out
}

// ─── Reviewer call ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ReviewerVerdict {
    verdict: String, // "accept" | "reject"
    #[serde(default)]
    summary: String,
    #[serde(default)]
    gaps: Vec<Gap>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Gap {
    #[serde(default)]
    area: String,
    #[serde(default)]
    what_missing: String,
    #[serde(default)]
    suggested_action: String,
}

#[derive(Debug, Clone)]
struct RejectionMemo {
    summary: String,
    gaps: Vec<Gap>,
    /// The actual artifact text the agent produced last iter (e.g.,
    /// the PLAN.md it wrote). Pasted back into the next-iter prompt
    /// so the model can pattern-match its own mistake against the
    /// required shape, rather than just being told "you got it
    /// wrong". Without this, deterministic-ish local-model inference
    /// produces the same wrong output every iter when the rejection
    /// summary is identical — observed 2026-05-12 codex smoke
    /// (prompt_sha unchanged across iters 2 + 3).
    prior_attempt_text: Option<String>,
    /// "Attempt N of max-iters". Escalates the prompt's seriousness
    /// across retries — a fresh "your last attempt was wrong" reads
    /// the same on attempt 1 and attempt 5; tagging the count gives
    /// the model an unambiguous signal.
    attempt_count: u32,
}

struct ReviewerInputs<'a> {
    artifacts: &'a ResolvedArtifacts,
    done_md: &'a str,
    diff_md: &'a str,
    workdir: &'a Path,
    iter: u32,
}

async fn call_reviewer(
    cfg: &RunCfg,
    inputs: &ReviewerInputs<'_>,
) -> Result<ReviewerVerdict, String> {
    let prompt = build_reviewer_prompt(inputs);
    let url = format!(
        "{}/v1/chat/completions",
        cfg.daemon_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": cfg.reviewer_model,
        "temperature": REVIEWER_TEMPERATURE,
        "top_p": 1.0,
        "max_tokens": REVIEWER_MAX_TOKENS,
        "messages": [
            {"role": "system", "content": REVIEWER_SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ],
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REVIEWER_TIMEOUT_S))
        .build()
        .map_err(|e| format!("build reqwest client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("daemon returned {status}: {}", truncate(&text, 1024)));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse daemon response JSON: {e}"))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.is_empty() {
        return Err("reviewer returned empty content".into());
    }

    let trimmed = strip_fences(&content);
    let verdict: ReviewerVerdict = serde_json::from_str(&trimmed).map_err(|e| {
        format!(
            "reviewer JSON parse failed: {e}; raw response: {}",
            truncate(&content, 2048)
        )
    })?;

    // Save reviewer transcript next to the verdict for audit.
    let transcript_path = sovereign_runs_dir()
        .join("transcripts")
        .join(format!(
            "iter-{:03}-reviewer-{}.json",
            inputs.iter,
            chrono::Utc::now().timestamp()
        ));
    if let Some(parent) = transcript_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &transcript_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "request": body,
            "response_content": content,
            "parsed": &verdict,
            "workdir": inputs.workdir.display().to_string(),
        }))
        .unwrap_or_default(),
    );

    Ok(verdict)
}

fn build_reviewer_prompt(inputs: &ReviewerInputs<'_>) -> String {
    let mut out = String::new();
    out.push_str("You are reviewing whether an agent's claim of completion meets the project's contract.\n\n");

    if let Some(charter) = inputs.artifacts.charter.as_ref() {
        out.push_str(&format!(
            "=== CHARTER ({}) — the rubric for what \"done\" means here ===\n",
            charter.label
        ));
        out.push_str(&charter.body);
        out.push_str("\n\n");
    } else {
        out.push_str("=== CHARTER ===\n(none provided — fall back to design-fidelity only)\n\n");
    }

    if let Some(design) = inputs.artifacts.design.as_ref() {
        out.push_str(&format!("=== DESIGN ({}) — the technical contract ===\n", design.label));
        out.push_str(&design.body);
        out.push_str("\n\n");
    }

    if let Some(plan) = inputs.artifacts.plan.as_ref() {
        out.push_str(&format!("=== PLAN ({}) — the scope ===\n", plan.label));
        out.push_str(&plan.body);
        out.push_str("\n\n");
    }

    out.push_str("=== AGENT'S DONE.md (claim of completion) ===\n");
    out.push_str(inputs.done_md);
    out.push_str("\n\n");

    let diff_truncated = truncate(inputs.diff_md, DIFF_BYTE_BUDGET);
    out.push_str("=== git diff since iteration start (what actually changed) ===\n");
    if diff_truncated.is_empty() {
        out.push_str("(no diff available — workdir not a git repo or no commits)\n\n");
    } else {
        out.push_str(&diff_truncated);
        out.push_str("\n\n");
    }

    out.push_str(
        "Decide: does the agent's DONE claim, supported by the diff, satisfy the charter \
         and design? Output ONE valid JSON object exactly matching this schema (no prose, \
         no markdown fences):\n\n\
         {\n\
         \x20 \"verdict\": \"accept\" | \"reject\",\n\
         \x20 \"summary\": \"one paragraph — what's good, what's missing, the bottom line\",\n\
         \x20 \"gaps\": [\n\
         \x20   {\n\
         \x20     \"area\": \"short label, e.g. 'forward compat' or 'phase 3 scoring'\",\n\
         \x20     \"what_missing\": \"specific thing the design/charter requires that DONE does not show\",\n\
         \x20     \"suggested_action\": \"concrete next step the agent should take\"\n\
         \x20   }\n\
         \x20 ]\n\
         }\n\
         \n\
         Bias-aware notes:\n\
         - If the agent's DONE.md honestly lists punts in 'What I did NOT do' AND those punts are \
           acceptable per the charter (e.g. charter says 'tests optional'), accept.\n\
         - If DONE lists punts that the charter or design treats as load-bearing, reject and \
           cite the specific section that was punted.\n\
         - The diff is ground truth. If DONE claims something the diff does not show, treat it \
           as missing — no benefit of the doubt.\n\
         - 'gaps' may be empty when verdict is 'accept'. It must be non-empty when verdict is 'reject'.\n",
    );

    out
}

const REVIEWER_SYSTEM_PROMPT: &str = "You output exactly one JSON object matching the requested schema. No prose. No markdown fences. The schema is strict.";

// ─── Phase: PLAN ─────────────────────────────────────────────────────────────

fn build_plan_prompt(
    artifacts: &ResolvedArtifacts,
    feature_id: &str,
    workdir: &Path,
    last_rejection: Option<&RejectionMemo>,
) -> String {
    let mut out = String::new();
    out.push_str("# ATOS run — PLAN phase\n\n");

    // Surface the prior iteration's rejection BEFORE the framing so
    // the agent sees feedback first. Without this the next iteration
    // sends the same prompt and makes the same mistake.
    //
    // What goes in the block (in order of pattern-match strength for
    // local models):
    //   1. Attempt counter — escalates seriousness across retries.
    //   2. The agent's OWN PRIOR OUTPUT — verbatim. Without this the
    //      model is told "you got it wrong" without seeing what wrong
    //      looked like. With temperature ≈ 0 it then deterministically
    //      reproduces the same wrong output every iter.
    //   3. The structured gap list — what was missing + suggested
    //      action per gap.
    //   4. A one-line "do not repeat" hammer at the end.
    //
    // The literal example of the required shape comes later in the
    // "Required structure" code block — the rejection block is the
    // contrast pass: "you wrote X; the required shape is shown below;
    // emit it as below, not as X".
    if let Some(memo) = last_rejection {
        out.push_str("## PREVIOUS ATTEMPT REJECTED — read this first\n\n");
        out.push_str(&format!(
            "**Attempt {}** — your last PLAN.md was rejected.\n\n",
            memo.attempt_count
        ));
        out.push_str(&memo.summary);
        out.push_str("\n\n");
        if let Some(prior) = &memo.prior_attempt_text {
            // Fence the prior emit so its markdown can't break out of
            // the prompt's own markdown frame. Use a backtick fence
            // wide enough to escape any reasonable nesting.
            out.push_str("### What you wrote last time (pasted back so you can see the shape problem)\n\n");
            out.push_str("````markdown\n");
            out.push_str(prior);
            if !prior.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("````\n\n");
            out.push_str(
                "That shape does NOT match the Required structure below. \
                 Read both, identify the surface-format difference, and write \
                 PLAN.md in the Required structure shape — not the shape above.\n\n",
            );
        }
        if !memo.gaps.is_empty() {
            out.push_str("Specific gaps:\n\n");
            for gap in &memo.gaps {
                out.push_str(&format!(
                    "- **{}** — {}. Suggested fix: {}\n",
                    gap.area, gap.what_missing, gap.suggested_action
                ));
            }
            out.push('\n');
        }
        out.push_str(
            "Do NOT repeat the previous mistake. Re-read the Required structure section below \
             and write PLAN.md from scratch in that exact shape.\n\n",
        );
    }

    out.push_str(&format!(
        "You are planning the implementation. **Write `{0}/PLAN.md`** to disk \
         — markdown is the medium, not JSON. Use whatever file-write tool \
         your harness exposes: in claude / opencode that's the `write` tool; \
         in codex that's `apply_patch` (or, if simpler, the `shell` / \
         `exec_command` tool with `cat > PLAN.md <<'EOF' … EOF`). The runner \
         reads `PLAN.md` after you exit and acts on the step list. The file \
         MUST exist on disk before you exit — if your last tool call \
         returned `unsupported call: <name>`, the file did not write and you \
         must retry with a different tool. Do not just describe the plan in \
         your message body; the runner reads the file, not the chat.\n\n",
        workdir.display(),
    ));
    out.push_str(&format!(
        "## Required structure\n\n\
         ```markdown\n\
         # Plan — feature {feature_id} · revision 1\n\
         \n\
         ## step-01: <one-sentence goal> [PENDING]\n\
         **Files:** `<source path>`, `<another path>`\n\
         **Verify:** `<shell command that runs a real test of this step's output>`\n\
         \n\
         <free-form rationale prose — why this step now, what it delivers>\n\
         \n\
         ## step-02: <next goal> [PENDING]\n\
         **Files:** `<source path>`\n\
         **Verify:** `<shell command exercising this step's behaviour>`\n\
         \n\
         <rationale>\n\
         ```\n\n\
         The structure is permissive — the parser tolerates extra prose, \
         missing optional fields, and stylistic variation. The hard \
         requirements are:\n\n\
         - One `## step-NN:` heading per step\n\
         - `[PENDING]` bracket marker on every step (the runner flips \
           it to `[DONE]` / `[FAILED]` / `[SKIPPED]` as work progresses)\n\
         - One `**Verify:**` line with a single backtick-wrapped \
           command and **nothing after the closing backtick on that \
           line** — no prose, no `(expects ...)`, no trailing remarks. \
           Put commentary in the rationale paragraph instead. The \
           parser extracts the command from inside the backticks; \
           anything after gets shell-injected and breaks the run.\n\n",
    ));
    out.push_str(
        "## Verify-command rigor (MOST IMPORTANT)\n\n\
         Each step's `verify_cmd` is the runner's only objective signal that \
         the step is *actually* delivered. A weak verify (one that passes \
         even if the step did nothing meaningful) lets the agent silently \
         no-op while the FSM credits forward motion. **Don't write weak \
         verifies**.\n\n\
         Rules:\n\n\
         1. **Verify must EXERCISE the step's deliverable, not just \
            syntax-check it.** A type definition's verify must run a test \
            that constructs and round-trips the type. A function's verify \
            must run a test that calls it and asserts the result. A scaffolding \
            step (project init) is the only step where a build/compile check \
            alone is acceptable.\n\
         2. **Every step except scaffold must add at least one assertive \
            test.** The step's `Files:` list should include both the source \
            file and its test file when applicable.\n\
         3. **Verify exit code 0 must mean the step's contract is met.** If \
            verify_cmd exits 0 on an empty implementation, it's wrong.\n\
         4. The runner caps each verify at 60s wall — keep tests fast.\n\
         5. **For Rust projects: every verify (except scaffold) MUST include \
            a `--` test-name filter after `--test`. ** The runner rejects \
            bare `cargo test --test test_foo` — it must be \
            `cargo test --test test_foo -- test_name`. This filter makes \
            cargo exit non-zero if the named test function doesn't exist, \
            closing the \"empty test file passes\" gap. Name your test \
            something descriptive of the specific contract being verified.\n\n\
         Concrete shape — pick the idiom for whatever language the design \
         calls for. Examples by ecosystem (replace with whatever your \
         project uses):\n\n\
         - Rust: `cargo test --test test_foo -- roundtrip_general_hint` \
           (NOT bare `cargo test --test test_foo`)\n\
         - Rust scaffold step: `cargo build -p <crate>` or \
           `cargo check -p <crate>` (no `--` filter needed). **Files \
           must include BOTH `Cargo.toml` AND `src/lib.rs`** — Rust's \
           `cargo check` fails if the lib entry-point is missing. A \
           scaffold step with `Files:` listing only `Cargo.toml` will \
           be rejected or will fail at runtime because cargo can't \
           resolve the crate.\n\
         - Python: `pytest tests/test_<step>.py -q -k test_name`\n\
         - JS/TS: `npm test -- <step.test.ts>` or `vitest run <path>`\n\
         - Go: `go test ./<pkg> -run TestStep<NN>`\n\
         - Any: a custom script the step itself writes that asserts the \
           step's behaviour and exits non-zero on failure.\n\n",
    );
    out.push_str(
         "## Constraints\n\n\
          - **3 to 12 steps**, 32 hard cap. Fewer = better. Each step adds a \
            FSM transition cost; consolidate what naturally ships together.\n\
          - **If the workdir has no build file** (Cargo.toml, package.json, \
            go.mod, etc.), step-01 MUST create one alongside a minimal \
            source stub. Later steps depend on the build system existing; \
            the runner validates that any referenced build tool has its \
            project file either already on disk or in step-01's \
            `Files:`. For existing projects with a build file already \
            present, this validation is a no-op.\n\
          - **Flat layout** — files at workdir root unless the language \
            ecosystem requires otherwise. `Verify:` runs from workdir root.\n\
          - **Be consistent across step verify_cmds.** Use the same \
            package/module name everywhere — no typo variants. The runner \
            auto-canonicalises some Rust-specific cases (`cargo check -p X` \
            against on-disk `Cargo.toml`) but you should not rely on that.\n\
          - Order by dependency: scaffold → primitives → behaviour → \
            integration. Each step builds on those before it.\n\n\
          ## Anti-patterns — what the validator will REJECT\n\n\
          - `cargo test --test test_foo` (bare, no `--` filter) — REJECTED. \
            Use `cargo test --test test_foo -- descriptive_test_name`.\n\
          - Files with inline commentary: ` **Files:** `lib.rs (Capability)`, \
            `tests/test_x.rs` ` — the parser will treat ` (Capability)` as \
            part of the filename. Write only the bare file path: \
            `**Files:** `lib.rs`, `tests/test_x.rs` `.\n\
          - Verifies that pass trivially: `echo ok`, `true`, `exit 0` — \
            REJECTED. The command must run a real check.\n\
          - Scaffold step listing only `Cargo.toml` without `src/lib.rs` — \
            `cargo check` fails if the lib entry-point is missing. Both \
            files are required for a minimal compilable Rust crate.\n\n",
    );

    if let Some(design) = artifacts.design.as_ref() {
        out.push_str(&format!("## Design ({})\n\n", design.label));
        out.push_str(design.body.trim());
        out.push_str("\n\n");
    }
    if let Some(charter) = artifacts.charter.as_ref() {
        out.push_str(&format!("## Charter ({})\n\n", charter.label));
        out.push_str(charter.body.trim());
        out.push_str("\n\n");
    }
    if let Some(plan) = artifacts.plan.as_ref() {
        out.push_str(&format!(
            "## Operator's hint about steps ({}) — content cues only, NOT format\n\n\
             The text below names what the operator thinks the steps cover. \
             USE IT for the step contents (what each step delivers) but DO NOT \
             preserve its surface format — your PLAN.md must follow the \
             `## step-NN:` heading shape from the Required structure section above, \
             regardless of whatever shape the hint below uses.\n\n",
            plan.label
        ));
        out.push_str(plan.body.trim());
        out.push_str("\n\n");
    }

    out.push_str(
        "Now: read the design, decide the steps, and `write` PLAN.md \
         with the EXACT structure shown in the Required structure section above \
         (`## step-NN: <goal> [PENDING]` headings, `**Files:**` + `**Verify:**` lines, \
         rationale paragraph). After PLAN.md is on disk, exit your session — the \
         runner takes over.\n",
    );
    out
}

/// Save a plan to both the run-scoped audit path AND the workdir-
/// scoped "live" path. Resumption only reads the workdir path, but
/// the run-scoped copy keeps the per-run history intact for audit.
/// Best-effort: a workdir write failure is logged but doesn't abort
/// the run — the run-scoped audit copy is the authoritative source
/// for in-flight reasoning.
fn save_plan_dual(
    plan: &Plan,
    run_path: &Path,
    workdir_path: &Path,
    workdir_md_path: &Path,
) -> Result<(), String> {
    plan.save(run_path)?;
    if let Some(parent) = workdir_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = plan.save(workdir_path) {
        eprintln!("atos run: workdir plan write warning: {e}");
    }
    // PLAN.md is the agent-facing source of truth (markdown pivot).
    // We re-render after every state transition so bracket markers
    // (`[PENDING]`/`[DONE]`/`[FAILED]`) stay in sync — without this,
    // the file last written during PLAN/REASSESS goes stale across
    // EXECUTE iters and undermines the operator's mental model.
    if let Err(e) = std::fs::write(workdir_md_path, render_plan_md(plan)) {
        eprintln!(
            "atos run: workdir PLAN.md write warning: {} ({e})",
            workdir_md_path.display()
        );
    }
    Ok(())
}

/// If `json` is a single-key object whose key is in `envelope_keys`,
/// return the value as a string. Otherwise return `json` unchanged.
/// Models occasionally wrap structured output in `{"plan": {...}}` or
/// `{"result": {...}}`; this function lets the parser tolerate the
/// envelope without rejecting an otherwise-valid plan.
fn unwrap_envelope(json: &str, envelope_keys: &[&str]) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return json.to_string(),
    };
    let Some(obj) = parsed.as_object() else {
        return json.to_string();
    };
    if obj.len() != 1 {
        return json.to_string();
    }
    let (k, v) = obj.iter().next().unwrap();
    if envelope_keys.iter().any(|e| e.eq_ignore_ascii_case(k)) {
        if let Ok(inner) = serde_json::to_string(v) {
            return inner;
        }
    }
    json.to_string()
}

/// Once Cargo.toml exists with a canonical `package.name`, walk the
/// plan and rewrite any `cargo check -p X` / `cargo test -p X` /
/// `cargo test --package X` whose `X` doesn't match the canonical
/// name. PLAN runs before any code exists, so the model has nothing
/// to ground crate-name choices in and reliably hallucinates typos
/// (`oicp_types`, `oicptypes`, `oictypes`). Post-processing the plan
/// against the on-disk Cargo.toml is cheap and deterministic — it
/// converts the model's drift into a non-issue without another
/// round-trip.
///
/// Returns true when at least one verify_cmd was rewritten.
fn canonicalize_verify_cmds(plan: &mut Plan, canonical_crate: &str) -> bool {
    let mut rewrote = false;
    for step in plan.steps.iter_mut() {
        let mut new_cmd = step.verify_cmd.clone();
        for prefix in ["cargo check -p ", "cargo test -p ", "cargo test --package "] {
            while let Some(idx) = new_cmd.find(prefix) {
                let name_start = idx + prefix.len();
                let after = &new_cmd[name_start..];
                let name_end = after
                    .find(|c: char| c.is_whitespace() || c == '&' || c == '|' || c == ';')
                    .unwrap_or(after.len());
                let current_name = &after[..name_end];
                if current_name != canonical_crate && !current_name.is_empty() {
                    let mut rebuilt = String::with_capacity(new_cmd.len());
                    rebuilt.push_str(&new_cmd[..name_start]);
                    rebuilt.push_str(canonical_crate);
                    rebuilt.push_str(&after[name_end..]);
                    new_cmd = rebuilt;
                } else {
                    break; // already canonical or empty — exit inner while
                }
            }
        }
        if new_cmd != step.verify_cmd {
            step.verify_cmd = new_cmd;
            rewrote = true;
        }
    }
    rewrote
}

/// Render a plan as markdown — the **canonical source of truth** the
/// agent reads and edits during PLAN / REASSESS phases. State markers
/// are bracketed words (`[PENDING]` / `[DONE]` / `[FAILED]` / etc.)
/// so a parser can round-trip the format losslessly without
/// guessing at unicode glyphs.
///
/// Format:
/// ```markdown
/// # Plan — feature foo · revision 1
///
/// ## step-01: <goal title> [PENDING]
/// **Files:** `path1`, `path2`
/// **Verify:** `cargo check`
///
/// <free-form rationale prose>
///
/// ## step-02: <next goal> [DONE]
/// ...
/// ```
///
/// The agent maintains this file using its standard `write` / `edit`
/// tools — no custom MCP needed. State transitions are bracket-text
/// swaps the agent can do reliably.
fn render_plan_md(plan: &Plan) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Plan — feature {} · revision {}\n\n",
        plan.feature_id, plan.revision
    ));
    out.push_str(
        "_Source of truth for the ATOS run. The runner reads this file \
         after each PLAN / REASSESS pass; edit step state by changing \
         the `[PENDING]` bracket marker to `[DONE]`, `[FAILED]`, or \
         `[SKIPPED]`. Add/remove steps by adding/removing `## step-NN:` \
         headings._\n\n",
    );
    for s in &plan.steps {
        let marker = state_marker(s.state);
        let goal_line = if s.goal.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", s.goal.trim())
        };
        out.push_str(&format!("## {}{} [{}]\n\n", s.id, goal_line, marker));
        if !s.files_touched.is_empty() {
            out.push_str(&format!("**Files:** `{}`\n", s.files_touched.join("`, `")));
        }
        if !s.verify_cmd.trim().is_empty() {
            out.push_str(&format!("**Verify:** `{}`\n", s.verify_cmd));
        }
        if !s.rationale.trim().is_empty() {
            out.push_str(&format!("\n{}\n", s.rationale.trim()));
        }
        if let Some(failure) = &s.last_failure {
            out.push_str(&format!(
                "\n<details><summary>last_failure ({} attempts)</summary>\n\n```\n{}\n```\n\n</details>\n",
                s.attempts,
                truncate(failure, 2000)
            ));
        }
        out.push('\n');
    }
    out
}

fn state_marker(state: StepState) -> &'static str {
    match state {
        StepState::Pending => "PENDING",
        StepState::InProgress => "IN_PROGRESS",
        StepState::Done => "DONE",
        StepState::Failed => "FAILED",
        StepState::Skipped => "SKIPPED",
    }
}

fn parse_state_marker(s: &str) -> Option<StepState> {
    match s.trim().to_ascii_uppercase().as_str() {
        "PENDING" | "TODO" | " " | "" => Some(StepState::Pending),
        "IN_PROGRESS" | "INPROGRESS" | "WIP" | "·" => Some(StepState::InProgress),
        "DONE" | "✓" | "X" => Some(StepState::Done),
        "FAILED" | "FAIL" | "✗" => Some(StepState::Failed),
        "SKIPPED" | "SKIP" | "⊘" => Some(StepState::Skipped),
        _ => None,
    }
}

/// Parse a markdown PLAN.md back into a `Plan`. The source-of-truth
/// pivot: instead of forcing the agent to emit JSON, the agent edits
/// PLAN.md (the format it produces fluently) and the runner reads
/// the bits it needs via heading-aware parsing.
///
/// Recognised structure:
///   - Top-line `# Plan — feature <id> · revision <N>` (optional;
///     defaults applied when absent).
///   - Each `## step-<id>[: <goal>] [STATE]` is a step.
///   - Inside a step section, `**Files:**`-prefixed line lists files
///     (comma-separated, optionally backtick-quoted).
///   - `**Verify:**`-prefixed line is the verify_cmd (typically
///     backtick-quoted).
///   - All other prose in the section becomes rationale.
///
/// Tolerant by design — missing fields default; unknown markers map
/// to Pending; case-insensitive state markers. The runner's
/// validation downstream catches truly-broken plans (empty steps,
/// missing verify_cmd on non-skipped step).
fn parse_plan_md(text: &str) -> Result<Plan, String> {
    let mut feature_id = String::new();
    let mut revision: u32 = 1;
    let mut current: Option<PendingStep> = None;
    let mut steps: Vec<Step> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        // Top-level header: `# Plan — feature <id> · revision <N>`
        if line.starts_with("# Plan") {
            if let Some(fid) = line.split("feature ").nth(1) {
                let mut s = fid.trim();
                // Trim trailing `· revision N` and friends.
                if let Some(idx) = s.find('·') {
                    s = &s[..idx];
                }
                feature_id = s.trim().to_string();
            }
            if let Some(rev) = line.split("revision ").nth(1) {
                if let Ok(n) = rev.trim().parse::<u32>() {
                    revision = n;
                }
            }
            continue;
        }
        // Step heading: `## step-NN[: goal] [STATE]`
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(prev) = current.take() {
                steps.push(prev.into_step());
            }
            let (head, marker) = split_state_marker(rest);
            const EM_DASH_SEP: &str = " — ";
            let (id, goal) = match head.find(':') {
                Some(i) => (head[..i].trim().to_string(), head[i + 1..].trim().to_string()),
                None => match head.find(EM_DASH_SEP) {
                    // Byte length of " — " is 5 (em-dash is 3 UTF-8
                    // bytes); use `.len()` not a hardcoded char count
                    // so the slice lands on a UTF-8 boundary.
                    Some(i) => (
                        head[..i].trim().to_string(),
                        head[i + EM_DASH_SEP.len()..].trim().to_string(),
                    ),
                    None => (head.trim().to_string(), String::new()),
                },
            };
            current = Some(PendingStep::new(
                id,
                goal,
                marker
                    .as_deref()
                    .and_then(parse_state_marker)
                    .unwrap_or(StepState::Pending),
            ));
            continue;
        }
        // Field lines or rationale, only meaningful inside a step.
        if let Some(step) = current.as_mut() {
            if let Some(payload) = line.strip_prefix("**Files:**") {
                step.files_touched = parse_inline_list(payload);
            } else if let Some(payload) = line.strip_prefix("**Verify:**") {
                step.verify_cmd = extract_verify_cmd(payload);
            } else if line.starts_with("<details>") || line.starts_with("</details>") {
                // Skip the runner-emitted last_failure expander; the
                // agent isn't supposed to authoritate on those.
            } else if !line.trim().is_empty()
                || (!step.rationale.is_empty() && !step.rationale.ends_with("\n\n"))
            {
                if !step.rationale.is_empty() {
                    step.rationale.push('\n');
                }
                step.rationale.push_str(line);
            }
        }
    }
    if let Some(prev) = current.take() {
        steps.push(prev.into_step());
    }

    if steps.is_empty() {
        return Err("PLAN.md has no `## step-XX:` headings".into());
    }

    Ok(Plan {
        schema_version: "1".into(),
        feature_id,
        design_sha: String::new(),
        created_at: String::new(),
        revision,
        steps,
    })
}

struct PendingStep {
    id: String,
    goal: String,
    state: StepState,
    files_touched: Vec<String>,
    verify_cmd: String,
    rationale: String,
}

impl PendingStep {
    fn new(id: String, goal: String, state: StepState) -> Self {
        Self {
            id,
            goal,
            state,
            files_touched: Vec::new(),
            verify_cmd: String::new(),
            rationale: String::new(),
        }
    }
    fn into_step(self) -> Step {
        Step {
            id: self.id,
            goal: self.goal,
            files_touched: self.files_touched,
            verify_cmd: self.verify_cmd,
            rationale: strip_failure_cruft(&self.rationale),
            state: self.state,
            attempts: 0,
            last_failure: None,
            last_verify_stdout: None,
        }
    }
}

/// Pull a fenced JSON block out of free-form agent text. Tries
/// (in order):
///   1. ` ```json ... ``` ` fenced block — strict.
///   2. Bare ` ``` ... ``` ` if body starts with `{` — common when
///      the model forgets the language tag.
///   3. The first balanced `{ ... }` span found via depth tracking
///      with string-literal awareness.
///
/// Used by PLAN / REASSESS to bypass the brittle
/// "tool-call-with-large-string-content" path. Returns None when no
/// JSON-looking content is present or extraction can't find a
/// balance point even with truncation tolerance.
fn extract_json_block(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = &text[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
        // Closing fence missing (model truncated). Try to recover via
        // brace-balancing on the body; if THAT fails too, return the
        // body as-is so the caller can surface a useful error.
        if let Some(recovered) = balance_braces(after.trim_start()) {
            return Some(recovered);
        }
        return Some(after.trim().to_string());
    }
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            let body = after[..end].trim();
            if body.starts_with('{') {
                return Some(body.to_string());
            }
        }
    }
    if let Some(start) = text.find('{') {
        if let Some(span) = balance_braces(&text[start..]) {
            return Some(span);
        }
    }
    None
}

/// Find the first balanced `{ ... }` span in `s`, returning it. Aware
/// of double-quoted strings (with backslash-escape) so braces inside
/// JSON string values don't fool the depth tracker. Returns None if
/// the depth never returns to zero (unterminated input).
fn balance_braces(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Read the most recent assistant message text for the given run from
/// opencode's database. Returns concatenated text from all `text` parts
/// of the latest assistant message in the latest session that targeted
/// this `run_id` (matched via the workdir we set as `directory`).
///
/// Best-effort: returns empty string when opencode's db is unreachable
/// or the schema doesn't match. The PLAN/REASSESS callers degrade
/// gracefully — a missing message just becomes a "plan_invalid"
/// outcome and the FSM retries.
fn fetch_last_assistant_text(workdir: &Path) -> String {
    let db = match dirs::home_dir() {
        Some(h) => h.join(".local/share/opencode/opencode.db"),
        None => return String::new(),
    };
    if !db.exists() {
        return String::new();
    }
    let conn = match rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    // Resolve the workdir's canonical path the way opencode stored it.
    // opencode uses /private-prefixed canonical paths on macOS.
    let canon = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
    let canon_str = canon.to_string_lossy().to_string();
    let alt_str = if canon_str.starts_with("/private/") {
        canon_str.trim_start_matches("/private").to_string()
    } else {
        format!("/private{canon_str}")
    };

    // Find the latest session for this directory. `directory` is a
    // top-level column on opencode's `session` table (not nested in
    // a JSON data blob like `message`/`part`), so it's queried as a
    // direct column. Both the user-typed and `/private/`-canonical
    // forms match — opencode normalises one way or the other
    // depending on the macOS path it was invoked with.
    let session_id: Option<String> = conn
        .query_row(
            "SELECT id FROM session WHERE directory IN (?, ?) ORDER BY time_created DESC LIMIT 1",
            rusqlite::params![canon_str, alt_str],
            |row| row.get(0),
        )
        .ok();
    let Some(sid) = session_id else {
        return String::new();
    };
    // Find the latest assistant message in that session.
    let msg_id: Option<String> = conn
        .query_row(
            "SELECT id FROM message WHERE session_id = ?1 AND json_extract(data,'$.role') = 'assistant' ORDER BY time_created DESC LIMIT 1",
            rusqlite::params![sid],
            |row| row.get(0),
        )
        .ok();
    let Some(mid) = msg_id else {
        return String::new();
    };
    let mut stmt = match conn.prepare(
        "SELECT json_extract(data,'$.text') FROM part WHERE message_id = ?1 AND json_extract(data,'$.type') = 'text' ORDER BY time_created",
    ) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let rows = match stmt.query_map(rusqlite::params![mid], |row| {
        let v: Option<String> = row.get(0)?;
        Ok(v.unwrap_or_default())
    }) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    let mut out = String::new();
    for r in rows.flatten() {
        out.push_str(&r);
        out.push('\n');
    }
    out
}

/// Outcome of looking for a `write` tool call to PLAN.md inside an
/// agent reply. Opencode stores tool calls as text parts wrapped in
/// `<tool_call>JSON</tool_call>`, but the text is often truncated
/// (observed at ~3800 bytes) — the opening tag is present but the
/// closing tag is cut off. When that happens, PLAN.md never lands on
/// disk and the caller needs to distinguish "agent never tried" from
/// "agent tried, payload was clipped" so it can produce a useful
/// failure message instead of the misleading "no fenced JSON block."
enum PlanFromAgent {
    /// No write-to-PLAN.md tool call appears in the text at all.
    NotPresent,
    /// A write-to-PLAN.md call is present but the wrapper was
    /// truncated and the payload is unrecoverable.
    Truncated,
    /// We extracted the plan markdown from the tool-call arguments.
    Markdown(String),
}

fn detect_write_tool_for_plan(text: &str) -> PlanFromAgent {
    if !text.contains("<tool_call>") {
        return PlanFromAgent::NotPresent;
    }
    let has_plan_write = text.contains("PLAN.md") && text.contains("\"write\"");
    if !has_plan_write {
        return PlanFromAgent::NotPresent;
    }
    if let Some(tag_start) = text.find("<tool_call>") {
        let json_start = tag_start + "<tool_call>".len();
        if let Some(tag_end) = text[json_start..].find("</tool_call>") {
            let json_str = &text[json_start..json_start + tag_end];
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(content) = val
                    .get("arguments")
                    .and_then(|a| a.get("content"))
                    .and_then(|v| v.as_str())
                {
                    return PlanFromAgent::Markdown(content.to_string());
                }
            }
        }
    }
    PlanFromAgent::Truncated
}

/// Operational metadata fields the agent typically omits when it
/// emits a plan. The runner backfills any empty field from these
/// defaults — `design_sha` is `None` for the PLAN phase (the agent
/// is expected to set it from the design body) and `Some(prior)`
/// for REASSESS (preserve the prior plan's value).
struct PlanDefaults {
    feature_id: String,
    design_sha: Option<String>,
    created_at: String,
}

/// Common parse path for a plan emitted via opencode's `write` tool
/// call (PLAN.md). Returns:
///   - `None` when no tool call was detected (caller should fall back
///     to fenced-JSON extraction);
///   - `Some(Err(...))` when a tool call was detected but parsing /
///     validation failed (this is a hard error — caller should not
///     fall back, the agent's plan is broken);
///   - `Some(Ok(plan))` on success.
fn try_parse_tool_emitted_plan(
    text: &str,
    defaults: &PlanDefaults,
    validate: bool,
) -> Option<Result<Plan, String>> {
    let md = match detect_write_tool_for_plan(text) {
        PlanFromAgent::NotPresent => return None,
        PlanFromAgent::Truncated => {
            return Some(Err(
                "detected write-to-PLAN.md tool call but the <tool_call> envelope was truncated by opencode (~3800 byte limit). Re-emit PLAN.md as a smaller payload.".into()
            ));
        }
        PlanFromAgent::Markdown(md) => md,
    };
    let mut parsed = match parse_plan_md(&md) {
        Ok(p) => p,
        Err(e) => return Some(Err(e)),
    };
    apply_plan_defaults(&mut parsed, defaults);
    if validate {
        if let Err(e) = parsed.validate() {
            return Some(Err(e));
        }
    }
    Some(Ok(parsed))
}

fn apply_plan_defaults(plan: &mut Plan, defaults: &PlanDefaults) {
    if plan.feature_id.trim().is_empty() {
        plan.feature_id = defaults.feature_id.clone();
    }
    if let Some(ds) = &defaults.design_sha {
        if plan.design_sha.trim().is_empty() {
            plan.design_sha = ds.clone();
        }
    }
    if plan.created_at.trim().is_empty() {
        plan.created_at = defaults.created_at.clone();
    }
}

/// Persist the agent's reply (PLAN or REASSESS) and pull a JSON block
/// out of it, validating against the Plan schema. Returns the extracted
/// Plan (with revision possibly bumped) ready to save.
fn extract_and_validate_plan(
    workdir: &Path,
    iter_dir: &Path,
) -> Result<Plan, String> {
    extract_and_validate_plan_with_defaults(workdir, iter_dir, "", "", "")
}

/// Variant used by REASSESS that pre-fills operational metadata fields
/// the agent typically omits (`feature_id`, `design_sha`, `created_at`)
/// from the prior plan. Without this, REASSESS-produced plans that
/// only emit structural fields (steps + revision) parse but fail
/// validation on the empty-feature_id check.
fn extract_and_validate_plan_with_defaults(
    workdir: &Path,
    iter_dir: &Path,
    fallback_feature_id: &str,
    fallback_design_sha: &str,
    fallback_created_at: &str,
) -> Result<Plan, String> {
    let text = fetch_last_assistant_text(workdir);
    if text.trim().is_empty() {
        return Err("agent reply was empty (no message in opencode db)".into());
    }
    let _ = std::fs::write(iter_dir.join("agent_reply.txt"), &text);
    let json = extract_json_block(&text)
        .ok_or_else(|| "agent reply had no fenced JSON block".to_string())?;
    // Some models wrap the plan in `{"plan": {...}}` instead of
    // emitting it as the top-level object. Auto-unwrap when we see
    // a single-key envelope so the parser doesn't fail on "missing
    // field `steps`" — the steps are one level deep.
    let json = unwrap_envelope(&json, &["plan", "implementation_plan", "result"]);
    let mut plan: Plan = serde_json::from_str(&json)
        .map_err(|e| format!("plan JSON parse: {e}; first 400: {}", truncate(&json, 400)))?;
    if plan.feature_id.trim().is_empty() {
        plan.feature_id = fallback_feature_id.to_string();
    }
    if plan.design_sha.trim().is_empty() {
        plan.design_sha = fallback_design_sha.to_string();
    }
    if plan.created_at.trim().is_empty() {
        plan.created_at = fallback_created_at.to_string();
    }
    plan.validate()?;
    Ok(plan)
}

// ─── Cross-iteration context (atos-context.md) ───────────────────────────────

/// Path to the workdir-resident operating context file. The runner
/// maintains this across iterations: a compact summary of what's in
/// the repo, the canonical crate name, and the most recent verdicts.
/// Every agent invocation gets it prepended so the agent doesn't have
/// to rediscover the workdir state with tool calls each time.
fn context_md_path(workdir: &Path) -> PathBuf {
    workdir.join(".sovereign").join("atos-context.md")
}

/// Cheap signal sources for the context: crate name, file inventory,
/// recent successful steps. Pure read-only over the workdir; no agent
/// involvement. Called before each phase so the prompt embeds
/// up-to-date facts.
fn refresh_atos_context(workdir: &Path, plan: Option<&Plan>) -> String {
    let mut out = String::new();
    out.push_str("# atos-context — operating memory\n\n");
    out.push_str("_Maintained by the runner; refreshed before each phase. The agent should treat this as ground truth — files described here exist, names quoted here are canonical._\n\n");

    // Canonical crate name from Cargo.toml. Captures the most common
    // failure mode: the planner hallucinates a typo'd crate name in a
    // verify_cmd, the agent uses that instead of the real one.
    out.push_str("## Canonical names\n\n");
    let cargo = workdir.join("Cargo.toml");
    if cargo.is_file() {
        let body = std::fs::read_to_string(&cargo).unwrap_or_default();
        let crate_name = parse_crate_name(&body).unwrap_or_else(|| "(unknown)".into());
        out.push_str(&format!(
            "- Cargo.toml `package.name` = `{crate_name}` — use this exact string in every `verify_cmd` and any `cargo check -p X` / `cargo test -p X`. Hyphens vs underscores matter; do not invent variants.\n",
        ));
    } else {
        out.push_str("- Cargo.toml: (not yet present — first step likely scaffolds it)\n");
    }
    out.push('\n');

    // File inventory at workdir root + src/. Caps the listing and
    // includes byte sizes so the agent sees "lib.rs is 0 bytes" and
    // knows the prior step left a hollow file.
    out.push_str("## File inventory (workdir + src/)\n\n");
    let mut entries: Vec<(String, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(workdir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push((name, size));
        }
    }
    if let Ok(rd) = std::fs::read_dir(workdir.join("src")) {
        for entry in rd.flatten() {
            let name = format!("src/{}", entry.file_name().to_string_lossy());
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push((name, size));
        }
    }
    entries.sort();
    if entries.is_empty() {
        out.push_str("- (workdir is empty)\n");
    } else {
        for (name, size) in entries.iter().take(40) {
            let marker = if *size == 0 { " ⚠ EMPTY" } else { "" };
            out.push_str(&format!("- `{name}` ({size} bytes){marker}\n"));
        }
    }
    out.push('\n');

    // Plan progress (if any). One line per step; lets the agent see
    // what's done without re-reading plan.json.
    if let Some(p) = plan {
        out.push_str(&format!(
            "## Plan progress (revision {})\n\n",
            p.revision
        ));
        for s in &p.steps {
            let marker = match s.state {
                StepState::Pending => "·",
                StepState::InProgress => "→",
                StepState::Done => "✓",
                StepState::Failed => "✗",
                StepState::Skipped => "⊘",
            };
            out.push_str(&format!(
                "- {marker} {} — {}\n",
                s.id,
                truncate(&s.goal, 80)
            ));
        }
        out.push('\n');
    }

    out
}

/// Best-effort extraction of `package.name` from a Cargo.toml. We
/// don't import `toml` for parsing here because Cargo.toml may be
/// malformed mid-edit — a permissive line scan is more resilient.
fn parse_crate_name(cargo_toml: &str) -> Option<String> {
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name") {
            let rest = rest.trim().trim_start_matches('=').trim();
            let rest = rest.trim_matches('"').trim_matches('\'');
            if !rest.is_empty() && !rest.contains(' ') {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Known failure patterns for the runner's pre-EXECUTE pattern-fix
/// helper. Each pattern is a substring match on `last_failure`; when
/// matched, the runner injects the corresponding `guidance` line into
/// the next EXECUTE prompt under "## Specific guidance for this
/// attempt". The model then sees a concrete instruction tied to the
/// exact error it produced last round, rather than a generic
/// "try again" framing.
///
/// Patterns are deliberately broad — substring matches over the
/// verbatim verify log catch most surface variants. False positives
/// are cheap (the agent ignores irrelevant guidance); false negatives
/// hurt (model spins on the same bug). Err on the side of inclusion.
struct PatternFix {
    needle: &'static str,
    guidance: &'static str,
}

const PATTERN_FIXES: &[PatternFix] = &[
    PatternFix {
        needle: "invalid type: map, expected a string",
        guidance: "Your Cargo.toml uses `[[X]]` (double brackets — TOML array-of-tables) where it should use `[X]` (single brackets — table). Sections like `[package]`, `[lib]`, `[dependencies]`, `[features]` are SINGLE-bracket tables. Only `[[bin]]`, `[[example]]`, `[[bench]]` are array-of-tables. Fix the section header.",
    },
    PatternFix {
        needle: "could not find `Cargo.toml`",
        guidance: "Cargo.toml must be at the **workdir root**, not inside a `<crate>/` subdirectory, and not at any other path. Use `write` with `filePath: \"Cargo.toml\"` (no leading directory).",
    },
    PatternFix {
        needle: "no targets specified in the manifest",
        guidance: "Your Cargo.toml has no library or binary target. Either create `src/lib.rs` (auto-detected as a library) or add `[lib]\\npath = \"src/lib.rs\"` to Cargo.toml.",
    },
    PatternFix {
        needle: "expected `;`",
        guidance: "Rust syntax error in `src/lib.rs` — likely a missing `;` after a statement or a stray expression. Check that every statement ends with `;` and every expression-as-block-tail does NOT.",
    },
    PatternFix {
        needle: "cannot find type",
        guidance: "Rust compile error: a referenced type isn't in scope. Either you forgot a `use` import, or you spelled the type wrong, or the type lives in a module path you haven't declared with `mod X;`.",
    },
    PatternFix {
        needle: "unresolved import",
        guidance: "An import doesn't resolve — the dependency is missing from Cargo.toml `[dependencies]`, or the path inside the crate is wrong. Add the dep or fix the path.",
    },
    PatternFix {
        needle: "expected struct, found",
        guidance: "Type mismatch: a struct expected somewhere a different shape was emitted. Often a serde rename or wrong field type. Check #[derive(Serialize, Deserialize)] and #[serde(rename = ...)] annotations.",
    },
    PatternFix {
        needle: "missing field",
        guidance: "Your code emitted a struct literal without one of its required fields. Either add the missing field or mark it `#[serde(default)]` if the field is optional in the wire format.",
    },
    PatternFix {
        needle: "no matching package named",
        guidance: "Your `verify_cmd` references a package name that doesn't match Cargo.toml's `package.name`. Either fix the verify_cmd to use the canonical name (the runner already canonicalizes `cargo check -p X` patterns; if you're still seeing this, the verify_cmd uses a different shape) or change Cargo.toml's name. The atos-context block above lists the canonical name verbatim.",
    },
];

fn pattern_fix_guidance(last_failure: Option<&str>) -> Option<String> {
    let Some(text) = last_failure else { return None; };
    let lower = text.to_lowercase();
    let mut hits: Vec<&PatternFix> = PATTERN_FIXES
        .iter()
        .filter(|p| lower.contains(&p.needle.to_lowercase()))
        .collect();
    if hits.is_empty() {
        return None;
    }
    // Cap at 3 hits to keep guidance focused; a verify log that
    // matches more than that is probably noisy.
    hits.truncate(3);
    let mut out = String::new();
    out.push_str("## Specific guidance for this attempt\n\n");
    out.push_str("The previous attempt failed with errors that match known patterns. Apply these fixes BEFORE rewriting:\n\n");
    for (i, fix) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, fix.guidance));
    }
    out.push('\n');
    Some(out)
}

/// Pre-seed a known-good Cargo.toml when the agent's step promises
/// to touch it but it's either missing or unparseable. Removes the
/// model's ability to break the `[package]` block — the agent only
/// needs to add `[dependencies]` entries afterwards.
///
/// Returns Some(path) when a write happened (so the runner can log
/// it), None when Cargo.toml already parses cleanly and was left
/// alone.
///
/// **Unwired 2026-05-06** — Rust-specific tooling coupling. Kept
/// available for explicit invocation if a Rust-only opt-in path is
/// reintroduced; not used by the FSM's hot path.
#[allow(dead_code)]
fn pre_seed_cargo_toml_if_needed(
    workdir: &Path,
    files_touched: &[String],
    crate_name_hint: &str,
) -> Option<PathBuf> {
    if !files_touched.iter().any(|f| f == "Cargo.toml") {
        return None;
    }
    let path = workdir.join("Cargo.toml");
    let needs_seed = match std::fs::read_to_string(&path) {
        Ok(body) => toml::from_str::<toml::Value>(&body).is_err(),
        Err(_) => true,
    };
    if !needs_seed {
        return None;
    }
    let safe_name = sanitize_crate_name(crate_name_hint);
    let body = format!(
        "[package]\n\
         name = \"{safe_name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         \n\
         [dependencies]\n\
         # The runner pre-seeded this skeleton because the previous\n\
         # attempt's Cargo.toml didn't parse. Add dependencies below.\n\
         # Don't move the [package] block — leave it as-is.\n",
    );
    if std::fs::write(&path, body).is_ok() {
        Some(path)
    } else {
        None
    }
}

/// Make a string Cargo-package-name-safe: lowercase, hyphens for
/// non-alphanumerics, no leading digits/dashes, length cap. Cargo's
/// rules are roughly `[a-z0-9_-]+` not starting with a digit and not
/// being a Cargo reserved word; this errs on the side of accepting.
#[allow(dead_code)]
fn sanitize_crate_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '-' || c.is_ascii_digit()).to_string();
    if trimmed.is_empty() {
        "atos-feature".into()
    } else if trimmed.len() > 64 {
        trimmed[..64].to_string()
    } else {
        trimmed
    }
}

// ─── Phase: EXECUTE ──────────────────────────────────────────────────────────

fn build_execute_prompt(
    step: &Step,
    plan: &Plan,
    workdir: &Path,
    diff_since_plan: &str,
    recent_notes: &str,
    atos_context: &str,
    design_body: Option<&str>,
) -> String {
    let mut out = String::new();
    if !atos_context.trim().is_empty() {
        out.push_str(atos_context);
        out.push_str("\n---\n\n");
    }
    out.push_str(&format!("# ATOS run — EXECUTE phase · {}\n\n", step.id));
    out.push_str(&format!("**Goal:** {}\n", step.goal));
    out.push_str(&format!("**Files this step touches:** {}\n", step.files_touched.join(", ")));
    out.push_str(&format!("**Verify command:** `{}`\n", step.verify_cmd));
    out.push_str(&format!("**Why this step:** {}\n\n", step.rationale));

    // Scaffold steps don't need the design spec — their job is
    // project skeleton, not spec implementation. Including 8KB of
    // irrelevant spec text consumes the agent's context budget and
    // correlates with the consistent step-01 failure pattern
    // (agent exits without writing files). For implementation
    // steps, the design stays — it was the breakthrough that got
    // step-02 passing (per HANDOFF 2026-05-06).
    let is_scaffold = step_goal_is_scaffold(&step.goal);
    if !is_scaffold {
        if let Some(design) = design_body {
            let snippet = if design.len() > 8000 {
                &design[..8000]
            } else {
                design
            };
            out.push_str("## Design (relevant excerpt)\n\n");
            out.push_str(snippet);
            out.push_str("\n\n");
        }
    }

    out.push_str(
         "## What you must NOT touch\n\n\
          These files are runner-managed. Editing them during EXECUTE \
          confuses the FSM and produces broken plan state — observed \
          failure mode where the model edits `PLAN.md` instead of the \
          source files declared above:\n\n\
          - `PLAN.md` — the runner rewrites this file from `plan.json` after \
            each PLAN/REASSESS phase. Your edits will be silently \
            overwritten next iter and waste your tool budget.\n\
          - `DESIGN.md` / `IMPLEMENTATION_PLAN.md` — frozen inputs.\n\
          - `.sovereign/` directory — runner state.\n\n\
          **Only write to the files listed above in `Files this step \
          touches:`**. If you think the plan needs to change, exit the \
          session without writing anything; the runner triggers REASSESS \
          after your exit and rewrites the plan there.\n\n\
          ## Tool schema (opencode driver)\n\n\
          The `read` tool requires `filePath` (not `path`). The `bash` \
          tool requires a `description` field. Using the wrong field \
          names causes the tool call to fail and wastes your turn budget. \
          Always include these required fields.\n\n",
    );

    out.push_str(
        "## How to know you're done\n\n\
         Run `verify_cmd` yourself with the bash tool after writing the \
         changes. The runner will run it again afterwards and only counts \
         the step as done when it exits zero. If your local run passes, \
         exit the session — the runner takes over from there.\n\n\
         **Critical:** the runner now gates step success on actually \
         modifying at least one of the files declared above. A passing \
         `verify_cmd` is not enough — you must use your `write` or \
         `edit` tool against the listed files. Exiting without writing \
         anything will be detected as a silent no-op and fail the step.\n\n\
         If verify fails after your changes and you can't make it pass in \
         this session, write a short `note` describing what you tried and \
         what's blocking. The runner will trigger a REASSESS pass that can \
         rewrite this step.\n\n",
    );

    if step.attempts > 0 {
        out.push_str(&format!(
            "## Previous attempt(s): {}\n\n",
            step.attempts
        ));
        if let Some(failure) = step.last_failure.as_ref() {
            out.push_str(&format!(
                "Last failure (truncated):\n```\n{}\n```\n\n",
                truncate(failure, 1500)
            ));
        }
        // Pattern-matched targeted guidance — overrides the generic
        // "take a different approach" framing when we recognise the
        // specific failure mode.
        if let Some(guidance) = pattern_fix_guidance(step.last_failure.as_deref()) {
            out.push_str(&guidance);
        } else {
            out.push_str(
                "Take a different approach this time — repeating the previous \
                 work won't unstick it.\n\n",
            );
        }
    }

    out.push_str("## Plan context — where this step fits\n\n");
    out.push_str(&format!("Plan revision: {}\n\n", plan.revision));
    out.push_str("Done so far:\n");
    let done: Vec<&Step> = plan.steps.iter().filter(|s| s.state == StepState::Done).collect();
    if done.is_empty() {
        out.push_str("- (none — this is the first executing step)\n");
    } else {
        for s in &done {
            out.push_str(&format!("- {}: {}\n", s.id, s.goal));
        }
    }
    out.push('\n');
    out.push_str("Still pending after this:\n");
    let after: Vec<&Step> = plan
        .steps
        .iter()
        .filter(|s| s.id != step.id && matches!(s.state, StepState::Pending | StepState::Failed))
        .collect();
    if after.is_empty() {
        out.push_str("- (none — this is the last step)\n\n");
    } else {
        for s in &after {
            out.push_str(&format!("- {}: {}\n", s.id, s.goal));
        }
        out.push('\n');
    }

    if !diff_since_plan.trim().is_empty() {
        out.push_str("## Repo state since the plan was written\n\n```diff\n");
        out.push_str(&truncate(diff_since_plan, 8 * 1024));
        out.push_str("\n```\n\n");
    } else {
        out.push_str("## Repo state\n\nWorkdir is at the plan-creation snapshot — no prior changes yet.\n\n");
    }

    if !recent_notes.trim().is_empty() {
        out.push_str("## Recent decision notes (for continuity)\n\n");
        out.push_str(recent_notes);
        out.push_str("\n\n");
    }

    out.push_str(&format!(
        "## Workdir\n\n`{}`\n\n**Files paths are RELATIVE to the workdir root** \
         (no leading `/`). For a single-crate Rust project, write \
         `Cargo.toml` and `src/lib.rs` at the workdir root — NOT inside a \
         `<name>/` subdirectory — so that `cargo check` from this directory \
         resolves the manifest. The runner's verify command runs from the \
         workdir root, not from a subdirectory.\n\n\
         Focus only on this step. Don't preview future steps; don't refactor \
         unrelated files. Use sovereign code-intel tools (`symbols`, \
         `code_search`) instead of reading whole files when you need to \
         navigate.\n",
        workdir.display(),
    ));
    out
}

// ─── Phase: REASSESS ─────────────────────────────────────────────────────────

fn build_reassess_prompt(
    plan: &Plan,
    trigger: &ReassessTrigger,
    diff_since_plan: &str,
    decision_summary: &str,
    last_rejection: Option<&RejectionMemo>,
    atos_context: &str,
    workdir: &Path,
) -> String {
    let mut out = String::new();
    if !atos_context.trim().is_empty() {
        out.push_str(atos_context);
        out.push_str("\n---\n\n");
    }
    out.push_str("# ATOS run — REASSESS phase\n\n");
    out.push_str(&format!(
        "Trigger: **{}**\n\n",
        match trigger {
            ReassessTrigger::Cadence => "scheduled (every K steps)",
            ReassessTrigger::StepFailure => "a step's verify_cmd failed and the agent couldn't unstick it",
            ReassessTrigger::ReviewerReject => "the reviewer rejected DONE.md — plan needs to address the gaps",
        }
    ));
    out.push_str(&format!(
        "Your job: rewrite `{0}/PLAN.md` with an updated plan. Use whatever \
         file-write tool your harness exposes: claude/opencode's `write` or \
         `edit`, codex's `apply_patch`, or a `shell`/`exec_command` heredoc — \
         the file MUST exist on disk before you exit. **Bump the revision** \
         in the top heading (e.g. `revision 2`). The runner reads the file \
         after you exit and merges execution state from the prior revision \
         (Done steps stay Done; Failed step counts carry over).\n\n\
         Allowed edits:\n\
         - Add new `## step-NN:` headings before/after existing ones\n\
         - Modify a remaining step's `**Verify:**` or `**Files:**` lines\n\
         - Mark a remaining step `[SKIPPED]` (with rationale in the body) \
           when it's revealed to be misconceived\n\
         - Reorder remaining steps\n\n\
         Do NOT modify steps already marked `[DONE]` — their verify \
         passed and the work is committed.\n\n",
        workdir.display(),
    ));
    out.push_str(
        "## Allowed mutations\n\n\
         - **Add new steps** before/after existing ones (use `step-NN` ids that \
           don't collide with existing).\n\
         - **Modify a remaining step** (state still `pending` or `failed`): \
           rewrite `goal`, `files_touched`, `verify_cmd`, or `rationale`.\n\
         - **Mark a remaining step `skipped`** with rationale captured in the \
           step's `last_failure` field — useful when REASSESS reveals the step \
           was misconceived.\n\
         - **Reorder remaining steps**.\n\n\
         ## Frozen — do not touch\n\n\
         Steps with `state: \"done\"` are immutable. Their `verify_cmd` passed \
         and the work is committed. Mutating them risks orphaning code that \
         later steps depend on.\n\n",
    );

    out.push_str(&format!("## Current plan state (revision {})\n\n", plan.revision));
    out.push_str("Compact summary — the runner already knows the execution history; do NOT include `last_failure`, `last_verify_stdout`, or `attempts` in your output JSON. Output `state` as `pending` or `skipped` only.\n\n");
    for s in &plan.steps {
        out.push_str(&format!(
            "- **{}** [{}] {}\n  files: `{}`\n  verify: `{}`\n",
            s.id,
            match s.state {
                StepState::Pending => "pending",
                StepState::InProgress => "in_progress",
                StepState::Done => "DONE (frozen)",
                StepState::Failed => "FAILED",
                StepState::Skipped => "skipped",
            },
            s.goal,
            s.files_touched.join("`, `"),
            s.verify_cmd,
        ));
        if matches!(s.state, StepState::Failed) {
            if let Some(failure) = &s.last_failure {
                out.push_str(&format!(
                    "  failed because: {}\n",
                    truncate(failure.lines().next().unwrap_or(""), 200)
                ));
            }
        }
    }
    out.push('\n');

    if let Some(rej) = last_rejection {
        out.push_str("## Reviewer rejection (you must address these gaps)\n\n");
        out.push_str(&rej.summary);
        out.push_str("\n\n");
        for g in &rej.gaps {
            out.push_str(&format!(
                "- **{}** — {}\n  *Suggested:* {}\n",
                g.area, g.what_missing, g.suggested_action
            ));
        }
        out.push('\n');
    }

    if !diff_since_plan.trim().is_empty() {
        out.push_str("## Repo diff since plan was written\n\n```diff\n");
        out.push_str(&truncate(diff_since_plan, 8 * 1024));
        out.push_str("\n```\n\n");
    }
    if !decision_summary.trim().is_empty() {
        out.push_str("## Decision notes captured during execution\n\n");
        out.push_str(decision_summary);
        out.push_str("\n\n");
    }
    out
}

// ─── Driver subprocess ───────────────────────────────────────────────────────

fn spawn_driver(
    driver: DriverKind,
    driver_model: &str,
    workdir: &Path,
    prompt: &str,
    feature_id: &str,
    run_id: &str,
) -> std::io::Result<std::process::ExitStatus> {
    // Both `claude --print` and `opencode run` take the message as a
    // positional argument. (Earlier sovereign-atos paths threaded the
    // prompt over stdin via a flag that opencode no longer recognises;
    // forwarding the prompt as argv survives across versions and
    // fits within macOS ARG_MAX for any reasonable prompt size.)
    //
    // **stdin must inherit the parent TTY**, not be /dev/null. opencode
    // (and claude) can prompt mid-session — for permission grants,
    // multi-choice, etc. — and a closed stdin causes the prompt to
    // wait forever. `--dangerously-skip-permissions` keeps opencode
    // from blocking on permission grants for a hands-off ATOS loop;
    // operators who want manual review can override the spawn flags
    // upstream once the ATOS_DRIVER_ARGS env hook is wired.
    match driver {
        DriverKind::Claude => std::process::Command::new("claude")
            .arg("--print")
            .arg("--dangerously-skip-permissions")
            .arg(prompt)
            .current_dir(workdir)
            .env("SOVEREIGN_FEATURE_ID", feature_id)
            .env("ATOS_RUN_ID", run_id)
            .env("ATOS_DRIVER", "claude")
            .env("ATOS_MODE", "normal")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status(),
        DriverKind::Opencode => std::process::Command::new("opencode")
            .arg("run")
            .arg("--model")
            .arg(driver_model)
            .arg("--print-logs")
            .arg("--log-level")
            .arg("INFO")
            .arg("--dangerously-skip-permissions")
            .arg(prompt)
            .current_dir(workdir)
            .env("SOVEREIGN_FEATURE_ID", feature_id)
            .env("ATOS_RUN_ID", run_id)
            .env("ATOS_DRIVER", "opencode")
            .env("ATOS_MODE", "normal")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status(),
        // Codex (openai/codex 0.130+) talks the OpenAI Responses API.
        // `--profile commonwealth` selects the local-mesh provider
        // declared in ~/.codex/config.toml (see invariant note on
        // /v1/responses adapter). `--skip-git-repo-check` lets the
        // ATOS run treat a sub-tree of a repo as the workdir; the
        // safety the check provides is already covered by the ATOS
        // runner's own workdir guard. Tool-loop fix prerequisite:
        // daemon `force_tool_calls=false` (else every turn forces a
        // grammar-locked tool envelope and codex never terminates).
        // `--dangerous-bypass-approval-and-sandbox` (codex 0.130 alias
        // for the older `--full-auto` flag) is required for a hands-
        // off run; manual approval blocks at the first tool call.
        DriverKind::Codex => std::process::Command::new("codex")
            .arg("exec")
            .arg("--profile")
            .arg("commonwealth")
            .arg("--skip-git-repo-check")
            .arg("--dangerously-bypass-approvals-and-sandbox")
            .arg(prompt)
            .current_dir(workdir)
            .env("SOVEREIGN_FEATURE_ID", feature_id)
            .env("ATOS_RUN_ID", run_id)
            .env("ATOS_DRIVER", "codex")
            .env("ATOS_MODE", "normal")
            // `COMMONWEALTH_API_KEY` matches the `env_key` declared on
            // the `commonwealth` provider in ~/.codex/config.toml; the
            // value is unused (local mesh doesn't auth) but codex
            // refuses to start without it.
            .env("COMMONWEALTH_API_KEY", "local")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status(),
    }
}

// ─── On-disk iteration log ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct IterationRecord {
    iter: u32,
    phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_id: Option<String>,
    started_at: String,
    ended_at: String,
    prompt_sha: String,
    opencode_exit: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    verify_passed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    done_present: Option<bool>,
    verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    gap_count: Option<u32>,
    wall_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewer_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

impl IterationRecord {
    fn new(iter: u32, phase: &str, started_at: &str) -> Self {
        Self {
            iter,
            phase: phase.into(),
            step_id: None,
            started_at: started_at.into(),
            ended_at: String::new(),
            prompt_sha: String::new(),
            opencode_exit: 0,
            verify_passed: None,
            done_present: None,
            verdict: String::new(),
            gap_count: None,
            wall_seconds: 0,
            reviewer_error: None,
            notes: None,
        }
    }
}

fn append_jsonl(path: &Path, row: &IterationRecord) -> Result<(), String> {
    use std::io::Write;
    let line = serde_json::to_string(row)
        .map_err(|e| format!("serialize iteration record: {e}"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

// ─── Filesystem + git helpers ────────────────────────────────────────────────

fn sovereign_runs_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign")
        .join("runs")
}

fn git_rev_parse_head(workdir: &Path) -> std::io::Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("rev-parse")
        .arg("HEAD")
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_diff_against(workdir: &Path, base_sha: &str) -> std::io::Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("diff")
        .arg(base_sha)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct StopConditionOutcome {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Run the charter's `stop_condition` shell command in the workdir.
/// Exit zero means the agent's claim is mechanically verified; non-zero
/// is a hard gate that the doc says short-circuits the reviewer pass.
///
/// Always reports a result — process-spawn failures land as exit_code
/// 127 with the OS error in `stderr`, mirroring the convention bash
/// uses for "command not found".
fn run_stop_condition(workdir: &Path, command: &str) -> StopConditionOutcome {
    let out = std::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(workdir)
        .output();
    match out {
        Ok(o) => StopConditionOutcome {
            exit_code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => StopConditionOutcome {
            exit_code: 127,
            stdout: String::new(),
            stderr: format!("failed to spawn stop_condition: {e}"),
        },
    }
}

fn strip_fences(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    trimmed.to_string()
}

// ─── Help ────────────────────────────────────────────────────────────────────

fn print_help() {
    eprintln!(
        "sovereign atos run — ralph-wiggum-style loop driver\n\
         \n\
         Spawns a coding agent (opencode by default), waits for it to\n\
         write DONE.md, has a reviewer judge that DONE against the\n\
         charter, and re-spawns the agent with feedback until accept.\n\
         \n\
         USAGE\n\
        \x20   sovereign atos run --workdir <path> [flags]\n\
         \n\
         FLAGS\n\
        \x20   --workdir <path>            Repo to work in (REQUIRED).\n\
        \x20   --design <path>             Design doc path. Default: workdir/DESIGN.md (auto).\n\
        \x20   --charter <path>            Charter path. Default: workdir/CHARTER.md (auto).\n\
        \x20   --plan <path>               Plan path. Default: workdir/IMPLEMENTATION_PLAN.md (auto).\n\
        \x20   --feature-id <id>           Bind to this feature row. Default: workdir basename.\n\
        \x20   --driver opencode|claude|codex  Default: opencode.\n\
        \x20   --driver-model <id>         Model passed to opencode/claude --model. Default: commonwealth/FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L.\n\
        \x20   --max-iters <n>             Safety cap. Default: 20.\n\
        \x20   --daemon-url <url>          Default: http://localhost:9741.\n\
        \x20   --reviewer-model <id>       Default: FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L.\n\
        \x20   --done-marker <path>        File the agent writes to claim done. Default: DONE.md.\n\
        \x20   --dry-run                   Compose iter-1 prompt and exit without spawning.\n\
        \n\
         OUTPUTS\n\
        \x20   ~/.sovereign/runs/<run-id>/iterations.jsonl\n\
        \x20   ~/.sovereign/runs/<run-id>/iter-NNN/{{prompt.md,verdict.json,DONE.rejected.md}}\n\
         \n\
         AUDIT\n\
        \x20   sovereign-eval finalize-run <run-id> --experiment-repo <workdir>\n\
        \x20   sovereign audit <feature-id>\n"
    );
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_artifacts() -> ResolvedArtifacts {
        ResolvedArtifacts {
            design: Some(NamedDoc {
                label: "DESIGN.md".into(),
                path: PathBuf::from("DESIGN.md"),
                body: "## Anchor\nbody.".into(),
            }),
            charter: Some(NamedDoc {
                label: "CHARTER.md".into(),
                path: PathBuf::from("CHARTER.md"),
                body: "tests required.".into(),
            }),
            plan: None,
        }
    }

    #[test]
    fn first_iter_prompt_contains_design_charter_and_done_contract() {
        let prompt = compose_agent_prompt(1, &mk_artifacts(), None);
        assert!(prompt.contains("Iteration:** 1"));
        assert!(prompt.contains("DONE contract"));
        assert!(prompt.contains("## Charter"));
        assert!(prompt.contains("## Design"));
        assert!(prompt.contains("Starting fresh"));
        assert!(!prompt.contains("Reviewer feedback from previous iteration"));
    }

    #[test]
    fn followup_iter_prompt_includes_rejection_and_continuation_framing() {
        let memo = RejectionMemo {
            summary: "missing the scoring helpers".into(),
            gaps: vec![Gap {
                area: "scoring".into(),
                what_missing: "score_request not implemented".into(),
                suggested_action: "add a free function in src/score.rs".into(),
            }],
            prior_attempt_text: None,
            attempt_count: 1,
        };
        let prompt = compose_agent_prompt(3, &mk_artifacts(), Some(&memo));
        assert!(prompt.contains("Iteration:** 3"));
        assert!(prompt.contains("Reviewer feedback from previous iteration"));
        assert!(prompt.contains("score_request not implemented"));
        assert!(prompt.contains("Continuing from current state"));
    }

    #[test]
    fn parses_required_workdir() {
        let err = RunCfg::from_args(&[]).unwrap_err();
        assert!(err.contains("--workdir"));
        let cfg = RunCfg::from_args(&["--workdir".into(), "/tmp".into()]).unwrap();
        assert_eq!(cfg.workdir, PathBuf::from("/tmp"));
        assert_eq!(cfg.max_iters, DEFAULT_MAX_ITERS);
        assert!(matches!(cfg.driver, DriverKind::Opencode));
        assert_eq!(cfg.driver_model, DEFAULT_DRIVER_MODEL);
    }

    #[test]
    fn parses_max_iters_and_driver_overrides() {
        let cfg = RunCfg::from_args(&[
            "--workdir".into(),
            "/tmp".into(),
            "--max-iters".into(),
            "3".into(),
            "--driver".into(),
            "claude".into(),
        ])
        .unwrap();
        assert_eq!(cfg.max_iters, 3);
        assert!(matches!(cfg.driver, DriverKind::Claude));
    }

    #[test]
    fn rejects_zero_max_iters() {
        let err = RunCfg::from_args(&[
            "--workdir".into(),
            "/tmp".into(),
            "--max-iters".into(),
            "0".into(),
        ])
        .unwrap_err();
        assert!(err.contains("must be > 0"));
    }

    #[test]
    fn validates_workdir_must_be_dir() {
        let cfg = RunCfg::from_args(&[
            "--workdir".into(),
            "/this/path/does/not/exist".into(),
        ])
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn strip_fences_handles_json_block() {
        assert_eq!(strip_fences("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences("```\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_fences("{\"a\": 1}"), "{\"a\": 1}");
    }

    #[test]
    fn resolve_artifacts_auto_discovers_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("DESIGN.md"), "design body").unwrap();
        std::fs::write(dir.join("CHARTER.md"), "charter body").unwrap();
        let cfg = RunCfg {
            workdir: dir.to_path_buf(),
            design_path: None,
            charter_path: None,
            plan_path: None,
            feature_id: None,
            driver: DriverKind::Opencode,
            driver_model: DEFAULT_DRIVER_MODEL.into(),
            max_iters: 1,
            daemon_url: DEFAULT_DAEMON_URL.into(),
            reviewer_model: DEFAULT_REVIEWER_MODEL.into(),
            done_marker: DEFAULT_DONE_MARKER.into(),
            dry_run: true,
            fresh_plan: false,
            show_help: false,
            accept: false,
        };
        let r = resolve_artifacts(&cfg).unwrap();
        assert!(r.design.is_some());
        assert!(r.charter.is_some());
        assert!(r.plan.is_none());
    }

    #[test]
    fn resolve_artifacts_errors_when_workdir_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = RunCfg {
            workdir: tmp.path().to_path_buf(),
            design_path: None,
            charter_path: None,
            plan_path: None,
            feature_id: None,
            driver: DriverKind::Opencode,
            driver_model: DEFAULT_DRIVER_MODEL.into(),
            max_iters: 1,
            daemon_url: DEFAULT_DAEMON_URL.into(),
            reviewer_model: DEFAULT_REVIEWER_MODEL.into(),
            done_marker: DEFAULT_DONE_MARKER.into(),
            dry_run: true,
            fresh_plan: false,
            show_help: false,
            accept: false,
        };
        let err = resolve_artifacts(&cfg).unwrap_err();
        assert!(err.contains("no DESIGN.md"));
    }

    #[test]
    fn iteration_record_serializes_compactly() {
        let mut row = IterationRecord::new(2, "execute(step-01)", "2026-05-05T00:00:00Z");
        row.ended_at = "2026-05-05T00:01:00Z".into();
        row.prompt_sha = "abc".into();
        row.verdict = "step_failed".into();
        row.verify_passed = Some(false);
        row.wall_seconds = 60;
        let s = serde_json::to_string(&row).unwrap();
        assert!(s.contains("\"verdict\":\"step_failed\""));
        assert!(s.contains("\"phase\":\"execute(step-01)\""));
        assert!(s.contains("\"verify_passed\":false"));
        // reviewer_error / done_present / gap_count / step_id / notes elided when None.
        assert!(!s.contains("reviewer_error"));
        assert!(!s.contains("done_present"));
        assert!(!s.contains("gap_count"));
        assert!(!s.contains("\"step_id\""));
        assert!(!s.contains("\"notes\""));
    }

    #[test]
    fn plan_validate_rejects_empty_steps() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "abc".into(),
            created_at: "2026".into(),
            revision: 1,
            steps: vec![],
        };
        assert!(plan.validate().unwrap_err().contains("zero steps"));
    }

    #[test]
    fn plan_validate_rejects_empty_verify_cmd() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "abc".into(),
            created_at: "2026".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "do x".into(),
                files_touched: vec!["a.rs".into()],
                verify_cmd: "  ".into(),
                rationale: "y".into(),
                state: StepState::Pending,
                attempts: 0,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        assert!(plan.validate().unwrap_err().contains("verify_cmd"));
    }

    #[test]
    fn decide_phase_starts_with_plan_when_no_plan_yet() {
        let p = decide_phase(None, None, 0, 3, false);
        assert!(matches!(p, Phase::Plan));
    }

    #[test]
    fn decide_phase_picks_execute_when_pending_step_exists() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "x".into(),
            created_at: "x".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "x".into(),
                files_touched: vec![],
                verify_cmd: "true".into(),
                rationale: "x".into(),
                state: StepState::Pending,
                attempts: 0,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        let p = decide_phase(Some(&plan), None, 0, 3, false);
        assert!(matches!(p, Phase::Execute(ref id) if id == "step-01"));
    }

    #[test]
    fn decide_phase_triggers_reassess_after_failure() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "x".into(),
            created_at: "x".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "x".into(),
                files_touched: vec![],
                verify_cmd: "false".into(),
                rationale: "x".into(),
                state: StepState::Failed,
                attempts: 1,
                last_failure: Some("nope".into()),
                last_verify_stdout: None,
            }],
        };
        let p = decide_phase(Some(&plan), None, 0, 3, true);
        assert!(matches!(p, Phase::Reassess(_)));
    }

    #[test]
    fn extract_json_block_finds_fenced_json() {
        let text = "Here's the plan:\n\n```json\n{\"a\": 1}\n```\n\nLet me know.";
        assert_eq!(extract_json_block(text), Some("{\"a\": 1}".into()));
    }

    #[test]
    fn extract_json_block_falls_back_to_balanced_braces() {
        let text = "I think the plan is { \"name\": \"x\", \"v\": 1 } that's it";
        let extracted = extract_json_block(text).unwrap();
        assert!(extracted.starts_with('{'));
        assert!(extracted.ends_with('}'));
    }

    #[test]
    fn extract_json_block_handles_nested_strings_with_braces() {
        // A pathological case the runner sees in practice: the agent
        // writes JSON whose string values contain `{` characters, e.g.
        // when describing code samples in `rationale`. The depth
        // tracker must ignore braces inside string literals.
        let text = "```json\n{\"rationale\": \"loop body: { x += 1; }\", \"v\": 1}\n```";
        let extracted = extract_json_block(text).unwrap();
        let v: serde_json::Value = serde_json::from_str(&extracted).unwrap();
        assert_eq!(v["v"], 1);
    }

    #[test]
    fn parse_crate_name_picks_up_package_name() {
        let cargo = r#"
[package]
name = "oicp-types"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = "1"
"#;
        assert_eq!(parse_crate_name(cargo), Some("oicp-types".into()));
    }

    #[test]
    fn parse_crate_name_returns_none_on_missing() {
        assert_eq!(parse_crate_name("[dependencies]\nfoo = \"1\""), None);
    }

    #[test]
    fn detect_hollow_files_flags_empty_lib_rs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        let warning = detect_hollow_files(
            tmp.path(),
            &["Cargo.toml".into(), "src/lib.rs".into()],
        );
        assert!(warning.is_some());
        let msg = warning.unwrap();
        assert!(msg.contains("src/lib.rs"));
    }

    #[test]
    fn detect_hollow_files_passes_substantive_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )
        .unwrap();
        let warning = detect_hollow_files(
            tmp.path(),
            &["Cargo.toml".into(), "src/lib.rs".into()],
        );
        assert!(warning.is_none());
    }

    #[test]
    fn detect_hollow_files_flags_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let warning =
            detect_hollow_files(tmp.path(), &["Cargo.toml".into(), "src/lib.rs".into()]);
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("missing"));
    }

    #[test]
    fn detect_untouched_files_flags_silent_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )
        .unwrap();
        let files = vec!["src/lib.rs".into()];
        // Snapshot mtime, do NOT touch the file, then check.
        let pre = snapshot_file_mtimes(tmp.path(), &files);
        // Sleep briefly so any subsequent write would have a
        // strictly newer mtime — proves the gate isn't matching by
        // accident on filesystem-equal timestamps.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let warning = detect_untouched_files(tmp.path(), &files, &pre);
        assert!(warning.is_some(), "expected silent-no-op to be detected");
        let msg = warning.unwrap();
        assert!(msg.contains("src/lib.rs"));
        assert!(msg.contains("no-op"));
    }

    #[test]
    fn detect_untouched_files_passes_when_file_modified() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "// before\n").unwrap();
        let files = vec!["src/lib.rs".into()];
        let pre = snapshot_file_mtimes(tmp.path(), &files);
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let warning = detect_untouched_files(tmp.path(), &files, &pre);
        assert!(warning.is_none());
    }

    #[test]
    fn detect_untouched_files_passes_when_file_newly_created() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        let files = vec!["src/lib.rs".into()];
        // Pre-snapshot before the file exists.
        let pre = snapshot_file_mtimes(tmp.path(), &files);
        assert!(pre[0].is_none(), "test setup: file must not exist yet");
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let warning = detect_untouched_files(tmp.path(), &files, &pre);
        assert!(warning.is_none());
    }

    #[test]
    fn detect_untouched_files_skips_when_files_list_empty_or_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty list — pure verify-only step.
        let pre = snapshot_file_mtimes(tmp.path(), &[]);
        assert!(detect_untouched_files(tmp.path(), &[], &pre).is_none());
        // "N/A" sentinel — runner should not gate on it.
        let na: Vec<String> = vec!["N/A".into()];
        let pre = snapshot_file_mtimes(tmp.path(), &na);
        assert!(detect_untouched_files(tmp.path(), &na, &pre).is_none());
        let na_lower: Vec<String> = vec!["n/a".into()];
        let pre = snapshot_file_mtimes(tmp.path(), &na_lower);
        assert!(detect_untouched_files(tmp.path(), &na_lower, &pre).is_none());
    }

    #[test]
    fn detect_untouched_files_passes_when_any_one_file_changes() {
        // Multi-file step where only one file actually got
        // modified — counts as forward motion.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "// stub\n").unwrap();
        let files: Vec<String> = vec!["Cargo.toml".into(), "src/lib.rs".into()];
        let pre = snapshot_file_mtimes(tmp.path(), &files);
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Only modify lib.rs.
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn z() {}\n").unwrap();
        let warning = detect_untouched_files(tmp.path(), &files, &pre);
        assert!(warning.is_none());
    }

    #[test]
    fn save_plan_dual_writes_markdown_alongside_json() {
        let tmp = tempfile::tempdir().unwrap();
        let run_path = tmp.path().join("run/plan.json");
        std::fs::create_dir_all(run_path.parent().unwrap()).unwrap();
        let workdir_path = tmp.path().join("workdir/.sovereign/plan.json");
        let workdir_md = tmp.path().join("workdir/PLAN.md");
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "feat".into(),
            design_sha: "abc".into(),
            created_at: "now".into(),
            revision: 2,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "do thing".into(),
                files_touched: vec!["src/lib.rs".into()],
                verify_cmd: "cargo check".into(),
                rationale: "because".into(),
                state: StepState::Done,
                attempts: 1,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        save_plan_dual(&plan, &run_path, &workdir_path, &workdir_md).unwrap();
        let md = std::fs::read_to_string(&workdir_md).unwrap();
        assert!(md.contains("step-01"));
        assert!(md.contains("[DONE]"), "markdown must reflect post-EXECUTE state, got:\n{md}");
        assert!(md.contains("revision 2"));
    }

    #[test]
    fn extract_verify_cmd_strips_trailing_commentary() {
        // Regression for 2026-05-06 smoke 3: agents append
        // commentary after the closing backtick. The naive
        // trim_matches('`') left the inner backtick + prose in the
        // verify_cmd, which broke shell execution.
        let cases = [
            (
                " `cargo check -p oicp-types` (expects exit code 0 after subsequent build)`",
                "cargo check -p oicp-types",
            ),
            (
                " `cargo test --test capability_hint_test` (expects failure if X)`",
                "cargo test --test capability_hint_test",
            ),
            // Clean case — single backticked chunk, no commentary.
            (" `cargo check`", "cargo check"),
            // No backticks at all — fall back to trim.
            (" cargo check ", "cargo check"),
            // Backticks but unbalanced — fall back to trim_matches.
            (" cargo check `", "cargo check"),
        ];
        for (input, expected) in cases {
            let got = extract_verify_cmd(input);
            assert_eq!(got, expected, "input: {input:?}");
        }
    }

    #[test]
    fn validate_rejects_non_ascii_verify_cmd() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "feat".into(),
            design_sha: "abc".into(),
            created_at: "now".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "Add types".into(),
                files_touched: vec!["src/lib.rs".into()],
                verify_cmd: "cargo test --test test_硬_gate -- foo".into(),
                rationale: "".into(),
                state: StepState::Pending,
                attempts: 0,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        let err = plan.validate().unwrap_err();
        assert!(err.contains("non-ASCII"), "expected non-ASCII rejection, got: {err}");
    }

    #[test]
    fn validate_rejects_prose_instead_of_file_path() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "feat".into(),
            design_sha: "abc".into(),
            created_at: "now".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "Add types".into(),
                files_touched: vec![
                    "tests for forward-compat deserialization of older peers' JSON".into(),
                ],
                verify_cmd: "cargo test --test test_foo -- bar".into(),
                rationale: "".into(),
                state: StepState::Pending,
                attempts: 0,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        let err = plan.validate().unwrap_err();
        assert!(err.contains("prose"), "expected prose rejection, got: {err}");
    }

    #[test]
    fn validate_accepts_normal_multi_word_file_paths() {
        // "src/some module/lib.rs" has 3 words — borderline but valid.
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "feat".into(),
            design_sha: "abc".into(),
            created_at: "now".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "Add types".into(),
                files_touched: vec!["src/some_module/lib.rs".into()],
                verify_cmd: "cargo test --test test_foo -- bar".into(),
                rationale: "".into(),
                state: StepState::Pending,
                attempts: 0,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn strip_failure_cruft_removes_embedded_failure_blocks() {
        // Simulates what REASSESS produces after multiple step-03
        // failures: the agent copies failure text into the rationale.
        let cases = [
            // Clean rationale — untouched.
            ("Implement three latency classes per spec.", "Implement three latency classes per spec."),
            // Failure block appended after rationale prose.
            ("Do the thing.\n\n```\nverify_cmd exited 0 but hollow-file gate failed: x\n\nverify output:\n\n---stderr---\nerror: ...\n```", "Do the thing."),
            // stderr dump.
            ("Add types.\n\n---stderr---\nerror: no test target", "Add types."),
            // <details> block.
            ("Setup project.\n\n<details><summary>last_failure (2 attempts)</summary>", "Setup project."),
            // "verify output:" line.
            ("Write tests.\n\nverify output:\n---stderr---", "Write tests."),
        ];
        for (input, expected) in cases {
            let got = strip_failure_cruft(input);
            assert_eq!(got, expected, "input: {input:?}");
        }
    }

    #[test]
    fn build_execute_prompt_warns_against_editing_runner_managed_files() {
        // Regression: 2026-05-06 smoke run #2 showed the agent
        // editing PLAN.md during EXECUTE instead of the declared
        // source files. The prompt now carries an explicit
        // do-not-touch list — confirm it's there so a future
        // refactor doesn't silently drop it.
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "feat".into(),
            design_sha: "abc".into(),
            created_at: "now".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "do the thing".into(),
                files_touched: vec!["src/lib.rs".into()],
                verify_cmd: "cargo check".into(),
                rationale: "because".into(),
                state: StepState::Pending,
                attempts: 0,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        let tmp = tempfile::tempdir().unwrap();
        let prompt = build_execute_prompt(&plan.steps[0], &plan, tmp.path(), "", "", "", None);
        assert!(
            prompt.contains("PLAN.md"),
            "EXECUTE prompt must explicitly mention PLAN.md as off-limits"
        );
        assert!(prompt.contains("DESIGN.md"));
        assert!(prompt.contains("runner-managed"));
        assert!(prompt.contains("silent no-op"));
    }

    #[test]
    fn pattern_fix_picks_up_lib_double_bracket_error() {
        let failure = "\n---stderr---\nerror: invalid type: map, expected a string\n --> Cargo.toml:8:1\n  |\n8 | [[lib]]\n";
        let guidance = pattern_fix_guidance(Some(failure)).unwrap();
        assert!(guidance.contains("Specific guidance"));
        assert!(guidance.contains("`[[X]]`"));
        assert!(guidance.contains("`[X]`"));
    }

    #[test]
    fn pattern_fix_returns_none_when_no_match() {
        let unrecognized = "\n---stderr---\nsomething weird happened in cargo land\n";
        assert!(pattern_fix_guidance(Some(unrecognized)).is_none());
    }

    #[test]
    fn pattern_fix_caps_at_three_hits() {
        // A failure that matches many patterns should still produce
        // a focused guidance (≤3 items).
        let failure = "could not find `Cargo.toml` and no targets specified in the manifest plus expected `;` here and cannot find type X and unresolved import Y";
        let guidance = pattern_fix_guidance(Some(failure)).unwrap();
        let bullet_count = guidance.matches('\n').count();
        assert!(bullet_count > 0);
        // Each bullet starts with "N. ", so count "1." through "3." appearances.
        assert!(guidance.contains("1."));
        assert!(guidance.contains("2."));
        assert!(guidance.contains("3."));
        assert!(!guidance.contains("4."));
    }

    #[test]
    fn pre_seed_cargo_toml_writes_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = pre_seed_cargo_toml_if_needed(
            tmp.path(),
            &["Cargo.toml".into()],
            "my-crate",
        );
        assert!(result.is_some());
        let body = std::fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(body.contains("name = \"my-crate\""));
        assert!(body.contains("[package]"));
        assert!(body.contains("[dependencies]"));
    }

    #[test]
    fn pre_seed_cargo_toml_replaces_unparseable() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = invalid-no-quotes\n",
        )
        .unwrap();
        let result = pre_seed_cargo_toml_if_needed(
            tmp.path(),
            &["Cargo.toml".into()],
            "x",
        );
        assert!(result.is_some());
        let body = std::fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(toml::from_str::<toml::Value>(&body).is_ok());
    }

    #[test]
    fn pre_seed_cargo_toml_leaves_valid_toml_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let original =
            "[package]\nname = \"keep-me\"\nversion = \"0.5.0\"\nedition = \"2021\"\n";
        std::fs::write(tmp.path().join("Cargo.toml"), original).unwrap();
        let result = pre_seed_cargo_toml_if_needed(
            tmp.path(),
            &["Cargo.toml".into()],
            "should-not-overwrite",
        );
        assert!(result.is_none());
        let body = std::fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert_eq!(body, original);
    }

    #[test]
    fn pre_seed_cargo_toml_skips_when_files_touched_excludes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let result = pre_seed_cargo_toml_if_needed(
            tmp.path(),
            &["src/lib.rs".into()],
            "x",
        );
        assert!(result.is_none());
        assert!(!tmp.path().join("Cargo.toml").exists());
    }

    #[test]
    fn sanitize_crate_name_handles_paths_and_symbols() {
        assert_eq!(sanitize_crate_name("my-crate"), "my-crate");
        assert_eq!(sanitize_crate_name("Foo Bar"), "foo-bar");
        assert_eq!(sanitize_crate_name("/tmp/abc.def"), "tmp-abc-def");
        assert_eq!(sanitize_crate_name("123-leading-digits"), "leading-digits");
        assert_eq!(sanitize_crate_name(""), "atos-feature");
    }

    #[test]
    fn unwrap_envelope_strips_plan_wrapper() {
        let json = r#"{"plan": {"schema_version": "1", "steps": [{"id": "s1"}]}}"#;
        let unwrapped = unwrap_envelope(json, &["plan"]);
        let v: serde_json::Value = serde_json::from_str(&unwrapped).unwrap();
        assert!(v.get("steps").is_some());
        assert!(v.get("plan").is_none());
    }

    #[test]
    fn unwrap_envelope_leaves_unwrapped_json_alone() {
        let json = r#"{"schema_version": "1", "steps": [{"id": "s1"}]}"#;
        let unwrapped = unwrap_envelope(json, &["plan"]);
        let v: serde_json::Value = serde_json::from_str(&unwrapped).unwrap();
        assert!(v.get("steps").is_some());
    }

    #[test]
    fn unwrap_envelope_leaves_multi_key_json_alone() {
        // Not a single-key wrapper, so don't unwrap.
        let json = r#"{"plan": {"x": 1}, "extra": "noise"}"#;
        let unwrapped = unwrap_envelope(json, &["plan"]);
        assert!(unwrapped.contains("\"plan\""));
        assert!(unwrapped.contains("\"extra\""));
    }

    #[test]
    fn canonicalize_verify_cmds_fixes_hyphen_underscore_drift() {
        let mut plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "x".into(),
            created_at: "x".into(),
            revision: 1,
            steps: vec![
                Step {
                    id: "s1".into(),
                    goal: "g".into(),
                    files_touched: vec![],
                    verify_cmd: "cargo check -p oicp-types".into(), // already canonical
                    rationale: "".into(),
                    state: StepState::Pending,
                    attempts: 0,
                    last_failure: None,
                    last_verify_stdout: None,
                },
                Step {
                    id: "s2".into(),
                    goal: "g".into(),
                    files_touched: vec![],
                    verify_cmd: "cargo check -p oicp_types".into(),
                    rationale: "".into(),
                    state: StepState::Pending,
                    attempts: 0,
                    last_failure: None,
                    last_verify_stdout: None,
                },
                Step {
                    id: "s3".into(),
                    goal: "g".into(),
                    files_touched: vec![],
                    verify_cmd: "cargo test --package oicptypes scoring".into(),
                    rationale: "".into(),
                    state: StepState::Pending,
                    attempts: 0,
                    last_failure: None,
                    last_verify_stdout: None,
                },
            ],
        };
        let rewrote = canonicalize_verify_cmds(&mut plan, "oicp-types");
        assert!(rewrote);
        assert_eq!(plan.steps[0].verify_cmd, "cargo check -p oicp-types");
        assert_eq!(plan.steps[1].verify_cmd, "cargo check -p oicp-types");
        assert_eq!(plan.steps[2].verify_cmd, "cargo test --package oicp-types scoring");
    }

    #[test]
    fn canonicalize_verify_cmds_noop_when_already_canonical() {
        let mut plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "x".into(),
            created_at: "x".into(),
            revision: 1,
            steps: vec![Step {
                id: "s1".into(),
                goal: "g".into(),
                files_touched: vec![],
                verify_cmd: "cargo check".into(),
                rationale: "".into(),
                state: StepState::Pending,
                attempts: 0,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        assert!(!canonicalize_verify_cmds(&mut plan, "oicp-types"));
    }

    #[test]
    fn refresh_atos_context_lists_canonical_crate_name() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"oicp-types\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let ctx = refresh_atos_context(tmp.path(), None);
        assert!(ctx.contains("`oicp-types`"));
        assert!(ctx.to_lowercase().contains("hyphens vs underscores matter"));
    }

    #[test]
    fn refresh_atos_context_marks_empty_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        let ctx = refresh_atos_context(tmp.path(), None);
        assert!(ctx.contains("EMPTY"));
        assert!(ctx.contains("src/lib.rs"));
    }

    #[test]
    fn parse_plan_md_round_trips_render() {
        let original = Plan {
            schema_version: "1".into(),
            feature_id: "fx".into(),
            design_sha: "abc".into(),
            created_at: "2026".into(),
            revision: 3,
            steps: vec![
                Step {
                    id: "step-01".into(),
                    goal: "Scaffold the crate".into(),
                    files_touched: vec!["Cargo.toml".into(), "src/lib.rs".into()],
                    verify_cmd: "cargo check".into(),
                    rationale: "Phase 0 — must build before types".into(),
                    state: StepState::Done,
                    attempts: 1,
                    last_failure: None,
                    last_verify_stdout: None,
                },
                Step {
                    id: "step-02".into(),
                    goal: "Wire core types".into(),
                    files_touched: vec!["src/lib.rs".into()],
                    verify_cmd: "cargo test --test types".into(),
                    rationale: "Phase 1 — types before behaviour".into(),
                    state: StepState::Pending,
                    attempts: 0,
                    last_failure: None,
                    last_verify_stdout: None,
                },
            ],
        };
        let md = render_plan_md(&original);
        assert!(md.contains("revision 3"));
        assert!(md.contains("step-01: Scaffold the crate [DONE]"));
        assert!(md.contains("step-02: Wire core types [PENDING]"));

        let parsed = parse_plan_md(&md).unwrap();
        assert_eq!(parsed.feature_id, "fx");
        assert_eq!(parsed.revision, 3);
        assert_eq!(parsed.steps.len(), 2);
        assert_eq!(parsed.steps[0].id, "step-01");
        assert_eq!(parsed.steps[0].state, StepState::Done);
        assert_eq!(parsed.steps[0].verify_cmd, "cargo check");
        assert_eq!(
            parsed.steps[0].files_touched,
            vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()]
        );
        assert!(parsed.steps[0].rationale.contains("Phase 0"));
        assert_eq!(parsed.steps[1].state, StepState::Pending);
        assert_eq!(parsed.steps[1].verify_cmd, "cargo test --test types");
    }

    #[test]
    fn parse_plan_md_handles_missing_optional_fields() {
        // The agent might emit a minimal step with no Files: line
        // and free-form rationale. Should still parse.
        let text = "\
# Plan — feature x · revision 1

## step-01: Bootstrap [PENDING]
**Verify:** `cargo check`

Build the skeleton and confirm it compiles.
";
        let parsed = parse_plan_md(text).unwrap();
        assert_eq!(parsed.steps.len(), 1);
        assert_eq!(parsed.steps[0].id, "step-01");
        assert_eq!(parsed.steps[0].verify_cmd, "cargo check");
        assert!(parsed.steps[0].files_touched.is_empty());
        assert!(parsed.steps[0].rationale.contains("Build the skeleton"));
    }

    #[test]
    fn parse_plan_md_tolerates_em_dash_separator() {
        let text = "\
# Plan — feature x · revision 2

## step-01 — Goal text [DONE]
**Verify:** `true`

prose
";
        let parsed = parse_plan_md(text).unwrap();
        assert_eq!(parsed.steps[0].id, "step-01");
        assert_eq!(parsed.steps[0].goal, "Goal text");
        assert_eq!(parsed.steps[0].state, StepState::Done);
    }

    #[test]
    fn parse_plan_md_rejects_empty_input() {
        assert!(parse_plan_md("# Plan\n\nno steps here\n").is_err());
    }

    #[test]
    fn parse_plan_md_recognises_state_synonyms() {
        let text = "\
# Plan — feature x · revision 1

## step-01: a [pending]
**Verify:** `t`

## step-02: b [DONE]
**Verify:** `t`

## step-03: c [Failed]
**Verify:** `t`

## step-04: d [skipped]
**Verify:** `t`

## step-05: e [in_progress]
**Verify:** `t`
";
        let parsed = parse_plan_md(text).unwrap();
        assert_eq!(parsed.steps[0].state, StepState::Pending);
        assert_eq!(parsed.steps[1].state, StepState::Done);
        assert_eq!(parsed.steps[2].state, StepState::Failed);
        assert_eq!(parsed.steps[3].state, StepState::Skipped);
        assert_eq!(parsed.steps[4].state, StepState::InProgress);
    }

    #[test]
    fn render_plan_md_includes_state_markers() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "abc".into(),
            created_at: "2026".into(),
            revision: 2,
            steps: vec![
                Step {
                    id: "step-01".into(),
                    goal: "scaffold".into(),
                    files_touched: vec!["Cargo.toml".into()],
                    verify_cmd: "cargo check".into(),
                    rationale: "phase 0".into(),
                    state: StepState::Done,
                    attempts: 1,
                    last_failure: None,
                    last_verify_stdout: None,
                },
                Step {
                    id: "step-02".into(),
                    goal: "types".into(),
                    files_touched: vec!["src/lib.rs".into()],
                    verify_cmd: "cargo test".into(),
                    rationale: "phase 1".into(),
                    state: StepState::Pending,
                    attempts: 0,
                    last_failure: None,
                    last_verify_stdout: None,
                },
            ],
        };
        let md = render_plan_md(&plan);
        assert!(md.contains("revision 2"));
        assert!(md.contains("step-01: scaffold [DONE]"));
        assert!(md.contains("step-02: types [PENDING]"));
        assert!(md.contains("`cargo check`"));
    }

    #[test]
    fn decide_phase_picks_final_when_all_done() {
        let plan = Plan {
            schema_version: "1".into(),
            feature_id: "f".into(),
            design_sha: "x".into(),
            created_at: "x".into(),
            revision: 1,
            steps: vec![Step {
                id: "step-01".into(),
                goal: "x".into(),
                files_touched: vec![],
                verify_cmd: "true".into(),
                rationale: "x".into(),
                state: StepState::Done,
                attempts: 1,
                last_failure: None,
                last_verify_stdout: None,
            }],
        };
        let p = decide_phase(Some(&plan), None, 0, 3, false);
        assert!(matches!(p, Phase::Final));
    }
}

