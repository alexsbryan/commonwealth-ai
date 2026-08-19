// SPDX-License-Identifier: AGPL-3.0-or-later
//! `agent-bench swebench` — drive the registered runners over SWE-bench
//! Verified instances and emit patches the official harness can grade.
//!
//! This is the arm that measures OUR code harness on the industry ruler.
//! `bare-metal` is the no-orchestration floor, `native` is the canonical
//! tool primitives; both are read the same way as the published
//! `mini-swe-agent` control and the `comaintainer` arm driven from
//! `sovereign/bench/external/swebench/arms/agentic.py`. Scoring happens
//! outside this binary — the seam is a unified diff per instance, so no
//! arm can be graded on terms another arm did not face.
//!
//! Deliberately NOT reusing `run_one_problem`: that path installs a
//! scaffold, copies `prompt.md`, then runs the witness and the judge.
//! Here the workdir is a real repository checkout and the grader is
//! external, so the only shared machinery is the runner registry and
//! `context_for` — which is exactly the machinery under test.
//!
//! Workdir mechanics: `context_for` owns a `TempDir`, so the checkout is
//! cloned INTO that tempdir with `git clone --shared` against a bare
//! cache (see `../bench/external/swebench/lib.py::ensure_bare`). Shared
//! object storage keeps this cheap; the bare repo must outlive the run.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

use commonwealth_agent_tools::{RoleModelMap, WorkdirScale};

use crate::problem::{
    BudgetCfg, Category, Problem, ProblemMeta, PromptCfg, ScoringCfg, ScoringDimCfg, ScoringMode,
    Tier, WitnessCfg, WitnessKind, WitnessLanguage,
};
use crate::runner::context_for;
use crate::runners::pi::PI_TOOL_ALLOWLIST;
use crate::runners::AgentRunnerRegistry;

#[derive(Debug, Error)]
pub enum SweError {
    #[error("unknown flag `{0}` (try --help)")]
    UnknownFlag(String),
    #[error("flag `{0}` requires a value")]
    MissingValue(String),
    #[error("unknown agent `{0}` (registered: {1})")]
    UnknownAgent(String, String),
    #[error("instances file not found: {0} — run prepare.py first")]
    NoInstances(PathBuf),
    #[error("prompt template missing: {0}")]
    NoPrompt(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("git {0} failed: {1}")]
    Git(String, String),
    #[error("{0} instances selected, {1} produced a prediction — a run that did no work is not a result (skipped: {2})")]
    NoWork(usize, usize, String),
    #[error("{CONTAINER_ENGINE} {0} failed: {1}")]
    Container(String, String),
}

/// Operator preference, 2026-08-18. `docker` works identically if set.
const CONTAINER_ENGINE: &str = "podman";

/// SWE-bench publishes x86_64 images only; they run under emulation on
/// arm64 (verified on this host, ~3.5s for a single-test run).
const PLATFORM: &str = "linux/amd64";

/// The working record written by `prepare.py`. The gold patch is held
/// out in `gold/gold.jsonl` and is deliberately absent here.
#[derive(Debug, Clone, Deserialize)]
pub struct Instance {
    pub instance_id: String,
    pub repo: String,
    pub base_commit: String,
    pub problem_statement: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub difficulty: String,
}

impl Instance {
    fn slug(&self) -> String {
        self.repo.replace('/', "__")
    }
}

#[derive(Debug, Serialize)]
struct Prediction {
    instance_id: String,
    model_name_or_path: String,
    model_patch: String,
}

#[derive(Debug, Serialize)]
struct RunLog {
    instance_id: String,
    arm: String,
    model: String,
    exit_reason: String,
    empty_patch: bool,
    patch_bytes: usize,
    input_tokens: u64,
    output_tokens: u64,
    wall_seconds: f64,
}

pub struct SweArgs {
    pub root: PathBuf,
    pub agent: String,
    pub model: String,
    pub limit: Option<usize>,
    pub only: Option<Vec<String>>,
    pub token_cap: u64,
    pub wall_seconds_cap: u64,
    pub verify_cmd: String,
    pub resume: bool,
}

impl SweArgs {
    pub fn parse(argv: &[String]) -> Result<Self, SweError> {
        let mut a = SweArgs {
            root: PathBuf::from("sovereign/bench/external/swebench"),
            agent: "native".to_string(),
            model: "commonwealth/coder".to_string(),
            limit: None,
            only: None,
            token_cap: 250_000,
            wall_seconds_cap: 1_800,
            // Runs INSIDE the instance image (see `verify_cmd_for`), so
            // this is the repo's own suite against its real dependencies
            // — the same footing every published SWE-bench number is
            // measured on. `-x -q` keeps the output small enough to feed
            // back to the model.
            verify_cmd: "cd /testbed && python -m pytest -x -q 2>&1 | tail -30".to_string(),
            resume: false,
        };
        let mut i = 0;
        while i < argv.len() {
            let flag = argv[i].as_str();
            let val = |i: &mut usize| -> Result<String, SweError> {
                *i += 1;
                argv.get(*i)
                    .cloned()
                    .ok_or_else(|| SweError::MissingValue(flag.to_string()))
            };
            match flag {
                "--root" => a.root = PathBuf::from(val(&mut i)?),
                "--agent" => a.agent = val(&mut i)?,
                "--model" => a.model = val(&mut i)?,
                "--limit" => {
                    a.limit = val(&mut i)?.parse().ok();
                }
                "--only" => {
                    a.only = Some(val(&mut i)?.split(',').map(|s| s.trim().to_string()).collect())
                }
                "--token-cap" => a.token_cap = val(&mut i)?.parse().unwrap_or(a.token_cap),
                "--wall-cap" => {
                    a.wall_seconds_cap = val(&mut i)?.parse().unwrap_or(a.wall_seconds_cap)
                }
                "--verify-cmd" => a.verify_cmd = val(&mut i)?,
                "--resume" => a.resume = true,
                other => return Err(SweError::UnknownFlag(other.to_string())),
            }
            i += 1;
        }
        Ok(a)
    }
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String, SweError> {
    let mut c = Command::new("git");
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c.output()?;
    if !out.status.success() {
        return Err(SweError::Git(
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).chars().take(2000).collect(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The official per-instance SWE-bench image. `__` becomes `_1776_`
/// because a docker tag cannot carry a double underscore.
fn image_for(instance_id: &str) -> String {
    format!(
        "docker.io/swebench/sweb.eval.x86_64.{}:latest",
        instance_id.replace("__", "_1776_")
    )
}

fn container(args: &[&str]) -> Result<String, SweError> {
    let out = Command::new(CONTAINER_ENGINE).args(args).output()?;
    if !out.status.success() {
        return Err(SweError::Container(
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).chars().take(2000).collect(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Materialise the instance's REAL environment into `dest`.
///
/// Not a git clone. Every published SWE-bench number is produced by an
/// agent working inside the per-instance image, where dependencies are
/// installed and the suite runs; a bare checkout is a strictly harder,
/// non-comparable variant (see `bench/external/README.md`). So the tree
/// is copied out of the image — `.git` and `.egg-info` included — and
/// the same directory is mounted back over `/testbed` for verification,
/// which is what `verify_cmd_for` builds.
///
/// `dest` must live under a path the container engine's VM shares with
/// the host; on macOS `/var/folders` (the default TempDir root) is NOT
/// shared, which is why callers create the tempdir under the bench root.
fn materialize_from_image(inst: &Instance, dest: &Path) -> Result<(), SweError> {
    let img = image_for(&inst.instance_id);
    let cid = container(&["create", "--platform", PLATFORM, &img, "sleep", "1"])?;
    let copy = container(&[
        "cp",
        &format!("{cid}:/testbed/."),
        &dest.to_string_lossy(),
    ]);
    let _ = container(&["rm", "-f", &cid]);
    copy?;
    // The image is checked out at its build commit, not the instance's.
    git(&["checkout", "--detach", "--quiet", &inst.base_commit], Some(dest))?;
    Ok(())
}

/// Verification runs INSIDE the instance image with the agent's working
/// tree mounted over `/testbed`, so the agent sees the same environment
/// the official grader will use.
fn verify_cmd_for(inst: &Instance, workdir: &Path, inner: &str) -> String {
    format!(
        "{CONTAINER_ENGINE} run --rm --platform {PLATFORM} -v {}:/testbed {} bash -lc {}",
        workdir.display(),
        image_for(&inst.instance_id),
        shell_quote(inner)
    )
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn extract_patch(workdir: &Path) -> Result<String, SweError> {
    git(&["add", "-A"], Some(workdir))?;
    git(&["diff", "--cached", "--no-color"], Some(workdir))
}

/// A `Problem` synthesized from a SWE-bench row. `Tier::FromScratch`
/// suppresses scaffold install and the `prompt.md` copy — neither
/// applies when the workdir is a real repository.
fn synth_problem(inst: &Instance, prompt_text: String, args: &SweArgs) -> Problem {
    let dim = |name: &str| ScoringDimCfg {
        name: name.to_string(),
        mode: ScoringMode::AutoTestPassFraction,
    };
    Problem {
        meta: ProblemMeta {
            id: inst.instance_id.clone(),
            title: format!("{} @ {}", inst.repo, &inst.base_commit[..12.min(inst.base_commit.len())]),
            category: Category::CodeTest,
            version: inst.version.clone(),
            notes: format!("SWE-bench Verified · difficulty {}", inst.difficulty),
            tier: Tier::FromScratch,
        },
        prompt: PromptCfg {
            file: "prompt.md".to_string(),
        },
        witness: WitnessCfg {
            kind: WitnessKind::AutoTestPass,
            language: WitnessLanguage::Python,
            fixture_subdir: String::new(),
            verify_cmd: args.verify_cmd.clone(),
            // Python: the executor no-ops the build primitive.
            build_cmd: Some("true".to_string()),
            scaffold_subdir: None,
            score_buckets: Vec::new(),
        },
        budget: BudgetCfg {
            token_cap: args.token_cap,
            wall_seconds_cap: args.wall_seconds_cap,
        },
        // Scoring is external (the official harness). These dims exist
        // only to satisfy the type; nothing in this path reads them.
        scoring: ScoringCfg {
            dim_a: dim("correctness"),
            dim_b: dim("approach"),
            dim_c: dim("efficiency"),
        },
        prompt_text,
        rubric_anchors: Default::default(),
        problem_dir: PathBuf::new(),
    }
}

fn render_prompt(
    template: &str,
    constraints: &str,
    inst: &Instance,
    verify_cmd: &str,
) -> String {
    let constraints = constraints.replace("{verify_cmd}", verify_cmd);
    let short = &inst.base_commit[..12.min(inst.base_commit.len())];
    template
        .replace("{repo}", &inst.repo)
        .replace("{commit}", short)
        .replace("{issue}", inst.problem_statement.trim())
        .replace("{constraints}", &constraints)
}

pub async fn run_command(argv: &[String]) -> Result<(), SweError> {
    let args = SweArgs::parse(argv)?;

    let instances_path = args.root.join("instances.jsonl");
    if !instances_path.is_file() {
        return Err(SweError::NoInstances(instances_path));
    }
    let flat_path = args.root.join("prompts/flat.md");
    let cons_path = args.root.join("prompts/constraints.md");
    for p in [&flat_path, &cons_path] {
        if !p.is_file() {
            return Err(SweError::NoPrompt(p.clone()));
        }
    }
    // The Rust arms are code harnesses, not seats — they get the same
    // `flat` framing as the control. The `order` framing belongs to the
    // comaintainer arm, which can actually delegate.
    let template = std::fs::read_to_string(&flat_path)?;
    let constraints = std::fs::read_to_string(&cons_path)?.trim().to_string();

    let registry = AgentRunnerRegistry::builtin();
    let runner = registry.get(&args.agent).ok_or_else(|| {
        SweError::UnknownAgent(args.agent.clone(), registry.agent_ids().join(", "))
    })?;

    let mut instances: Vec<Instance> = Vec::new();
    for line in std::fs::read_to_string(&instances_path)?.lines() {
        if !line.trim().is_empty() {
            instances.push(serde_json::from_str(line)?);
        }
    }
    if let Some(only) = &args.only {
        instances.retain(|i| only.contains(&i.instance_id));
    }
    if let Some(n) = args.limit {
        instances.truncate(n);
    }

    let preds_dir = args.root.join("preds").join(&args.agent);
    std::fs::create_dir_all(&preds_dir)?;
    let log_path = args.root.join("preds").join(format!("{}.runlog.jsonl", args.agent));

    let total = instances.len();
    let mut wrote = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    println!(
        "swebench: {total} instances · agent={} · model={} · caps={}tok/{}s",
        args.agent, args.model, args.token_cap, args.wall_seconds_cap
    );

    for (idx, inst) in instances.iter().enumerate() {
        let pred_path = preds_dir.join(format!("{}.json", inst.instance_id));
        if args.resume && pred_path.is_file() {
            println!("[{}/{total}] {} — already done", idx + 1, inst.instance_id);
            wrote += 1;
            continue;
        }
        println!("[{}/{total}] {} ({}) …", idx + 1, inst.instance_id, inst.difficulty);

        // Under the bench root, not /var/folders: the container engine's
        // VM does not share the macOS temp root, and the workdir has to
        // be bind-mountable for verification.
        let work_root = args.root.join("work");
        std::fs::create_dir_all(&work_root)?;
        let workdir = tempfile::Builder::new()
            .prefix(&format!("{}-", inst.instance_id))
            .tempdir_in(&work_root)?;
        if let Err(e) = materialize_from_image(inst, workdir.path()) {
            warn!(instance = %inst.instance_id, error = %e,
                  "swebench: could not materialise the instance image");
            skipped.push(format!("{} (image)", inst.instance_id));
            continue;
        }

        let inner_verify = verify_cmd_for(inst, workdir.path(), &args.verify_cmd);
        let prompt_text = render_prompt(&template, &constraints, inst, &inner_verify);
        let mut problem = synth_problem(inst, prompt_text, &args);
        problem.witness.verify_cmd = inner_verify;
        let ctx = context_for(
            &problem,
            workdir,
            PI_TOOL_ALLOWLIST,
            args.model.clone(),
            Some(args.token_cap),
            Some(args.wall_seconds_cap),
            RoleModelMap::new(),
        )
        // A repository is not a scaffold. This one declaration sizes
        // the preamble AND grants the read primitive the roles need:
        // turning the render off without the grant left the Implementer
        // blind and forced to write first (see WorkdirScale docs).
        .with_workdir_scale(WorkdirScale::Repository);

        let t0 = Instant::now();
        let artifact = match runner.run(ctx).await {
            Ok(a) => a,
            Err(e) => {
                warn!(instance = %inst.instance_id, error = %e, "swebench: runner error");
                skipped.push(format!("{} (runner)", inst.instance_id));
                continue;
            }
        };
        let elapsed = t0.elapsed().as_secs_f64();

        let wd = artifact.workdir_path();
        let patch = extract_patch(&wd).unwrap_or_else(|e| {
            warn!(instance = %inst.instance_id, error = %e, "swebench: diff failed");
            String::new()
        });

        std::fs::write(
            &pred_path,
            serde_json::to_string(&Prediction {
                instance_id: inst.instance_id.clone(),
                model_name_or_path: format!("{}:{}", args.agent, args.model),
                model_patch: patch.clone(),
            })? + "\n",
        )?;

        let log = RunLog {
            instance_id: inst.instance_id.clone(),
            arm: args.agent.clone(),
            model: args.model.clone(),
            exit_reason: format!("{:?}", artifact.exit_reason),
            empty_patch: patch.trim().is_empty(),
            patch_bytes: patch.len(),
            input_tokens: artifact.tokens.input,
            output_tokens: artifact.tokens.output,
            wall_seconds: (elapsed * 10.0).round() / 10.0,
        };
        info!(instance = %inst.instance_id, exit = %log.exit_reason,
              bytes = log.patch_bytes, "swebench: instance done");
        println!(
            "    {} · {:.1}s · {}B{}",
            log.exit_reason,
            elapsed,
            log.patch_bytes,
            if log.empty_patch { " EMPTY" } else { "" }
        );

        use std::io::Write as _;
        let mut fh = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        writeln!(fh, "{}", serde_json::to_string(&log)?)?;
        wrote += 1;
    }

    // A run that skipped everything must not exit 0. Same rule the test
    // wrapper enforces for `pass: 0 fail: 0` — absence is reported, never
    // defaulted into a green (ARCH_PRINCIPLES 18.2).
    if wrote == 0 && total > 0 {
        return Err(SweError::NoWork(total, wrote, skipped.join(", ")));
    }
    println!(
        "\n{wrote}/{total} predictions in {} ({} skipped)",
        preds_dir.display(),
        skipped.len()
    );
    if !skipped.is_empty() {
        println!("  skipped: {}", skipped.join(", "));
    }
    Ok(())
}
