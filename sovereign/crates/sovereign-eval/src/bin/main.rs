// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-eval` — operator-driven harness CLI.
//!
//! Subcommands:
//!   finalize-run <run-id>              dump manifest.json
//!   score <run-id>                     mechanical + judge + workflow + audit + scope + regression + tool grades
//!   diff <run-id-a> <run-id-b>         text diff across the two runs
//!   audit <run1-id> <run2-id>          audit-trail-only (run #1's notes vs run #2's queries)

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use sovereign_eval::{
    audit_trail, cognitive, diff as diff_mod, finalize, judge, manifest, mechanical, regression,
    scope, tool_grader, workflow,
};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "sovereign-eval",
    version,
    about = "Tool-efficacy self-host harness"
)]
struct Cli {
    /// Override the Sovereign data directory. Defaults to `$SOVEREIGN_DATA_DIR`,
    /// then `~/.svrnmesh/`.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mark a run as ended and dump its manifest.
    FinalizeRun {
        run_id: String,
        #[arg(long)]
        experiment_repo: Option<PathBuf>,
        #[arg(long, default_value = "http://localhost:9741")]
        daemon_url: String,
        #[arg(long)]
        out_dir: Option<PathBuf>,
        #[arg(long)]
        no_close: bool,
    },

    /// Run mechanical + judge + workflow + scope + regression + tool-grade analysis on a finalized run.
    /// Writes one JSON per analyzer alongside `manifest.json`. Audit-trail requires --against.
    Score(ScoreArgs),

    /// Text diff two scored runs.
    Diff {
        run_a: String,
        run_b: String,
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },

    /// Run audit-trail analysis on (earlier_run, later_run) pair.
    Audit {
        earlier_run: String,
        later_run: String,
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },

    /// Fast-tier cognitive unit-test bank against the Fast slot.
    /// Mechanical scoring only — no judge call. See
    /// `sovereign/inquiries/cognitive/` for the on-disk items.
    Cognitive(CognitiveArgs),
}

#[derive(Args, Debug)]
struct CognitiveArgs {
    /// Daemon base URL. /v1/chat/completions is appended.
    #[arg(long, default_value = "http://localhost:9741")]
    daemon_url: String,

    /// gguf file-stem of the model to invoke (NOT a slot alias like
    /// "fast" — the daemon resolves by stem, not slot abstraction).
    #[arg(long)]
    model: String,

    /// Optional category filter (situating_judgment, decision_quality,
    /// honesty_calibration, code_reasoning, charter_satisfaction).
    #[arg(long)]
    category: Option<String>,

    /// Optional single-item id filter — useful while authoring.
    #[arg(long)]
    item: Option<String>,

    /// Root of the cognitive bank. Defaults to
    /// `<workspace>/sovereign/inquiries/cognitive`.
    #[arg(long)]
    bank_root: Option<PathBuf>,

    /// Workspace root used to resolve relative `[[context_blocks]].file`
    /// references. Defaults to `cwd`.
    #[arg(long)]
    workspace_root: Option<PathBuf>,

    /// Where to write the JSON report. Defaults to
    /// `<data_dir>/runs/<auto-id>/cognitive.json`.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Path to a prior `cognitive.json`; render a baseline-diff in the
    /// text summary and embed it in the JSON report.
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Persist this run's report as the new baseline (writes a copy to
    /// the given path on success).
    #[arg(long)]
    save_baseline: Option<PathBuf>,

    /// Sampling temperature. Default matches `judge.rs` (0.0).
    #[arg(long, default_value_t = cognitive::runner::DEFAULT_TEMPERATURE)]
    temperature: f32,

    /// Decoding seed. Default mirrors `judge.rs:JUDGE_SEED` shape.
    #[arg(long, default_value_t = cognitive::runner::DEFAULT_SEED)]
    seed: u64,

    /// Max tokens per item response.
    #[arg(long, default_value_t = cognitive::runner::DEFAULT_MAX_TOKENS)]
    max_tokens: u32,

    /// Use the daemon's per-family sampling defaults (T/top_p/top_k
    /// from `ModelQuirks`) instead of `--temperature` / hardcoded
    /// `top_p=1.0`. Right for cross-family benchmarks where each
    /// model deserves its model-card recommended sampling. `--seed`
    /// still applies for reproducibility.
    #[arg(long)]
    family_defaults: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let data_dir = resolve_data_dir(cli.data_dir.as_deref())?;

    match cli.cmd {
        Cmd::FinalizeRun {
            run_id,
            experiment_repo,
            daemon_url,
            out_dir,
            no_close,
        } => cmd_finalize_run(
            &data_dir,
            &run_id,
            experiment_repo.as_deref(),
            &daemon_url,
            out_dir.as_deref(),
            no_close,
        ),
        Cmd::Score(args) => cmd_score_impl(&data_dir, args),
        Cmd::Diff {
            run_a,
            run_b,
            out_dir,
        } => cmd_diff(&data_dir, &run_a, &run_b, out_dir.as_deref()),
        Cmd::Audit {
            earlier_run,
            later_run,
            out_dir,
        } => cmd_audit(&data_dir, &earlier_run, &later_run, out_dir.as_deref()),
        Cmd::Cognitive(args) => cmd_cognitive(&data_dir, args),
    }
}

fn cmd_cognitive(data_dir: &Path, args: CognitiveArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("getting cwd")?;
    let workspace_root = args.workspace_root.unwrap_or(cwd);
    let bank_root = args
        .bank_root
        .unwrap_or_else(|| cognitive::default_bank_root(&workspace_root));

    let category_filter = match &args.category {
        Some(s) => Some(cognitive::Category::parse(s)?),
        None => None,
    };

    let report = cognitive::run_suite(cognitive::SuiteOpts {
        bank_root: &bank_root,
        workspace_root: &workspace_root,
        daemon_url: &args.daemon_url,
        model: &args.model,
        category_filter,
        item_id_filter: args.item.as_deref(),
        temperature: args.temperature,
        seed: args.seed,
        max_tokens: args.max_tokens,
        family_defaults: args.family_defaults,
    })?;

    let out_path = args.out.unwrap_or_else(|| {
        data_dir
            .join("runs")
            .join(&report.run_id)
            .join("cognitive.json")
    });
    write_pretty_json(&out_path, &report).context("writing cognitive report")?;

    let baseline = match &args.baseline {
        Some(p) => {
            let raw = std::fs::read_to_string(p)
                .with_context(|| format!("reading baseline {}", p.display()))?;
            let r: cognitive::Report = serde_json::from_str(&raw)
                .with_context(|| format!("parsing baseline {}", p.display()))?;
            Some(r)
        }
        None => None,
    };
    let diff = baseline
        .as_ref()
        .map(|b| cognitive::report::diff_baseline(b, &report));

    print!("{}", cognitive::report::render_text(&report, diff.as_ref()));
    println!("\n→ wrote {}", out_path.display());

    if let Some(save_path) = args.save_baseline {
        write_pretty_json(&save_path, &report)
            .with_context(|| format!("saving baseline to {}", save_path.display()))?;
        println!("→ saved baseline {}", save_path.display());
    }

    let total = report.items_total;
    let failed = total.saturating_sub(report.items_passed);
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[derive(Args, Debug)]
struct ScoreArgs {
    run_id: String,
    #[arg(long)]
    experiment_repo: PathBuf,
    #[arg(long, default_value = "http://localhost:9741")]
    daemon_url: String,
    #[arg(long)]
    out_dir: Option<PathBuf>,

    #[arg(long, help = "Run ID of an earlier run; enables audit-trail analysis")]
    against: Option<String>,

    #[arg(
        long,
        help = "Skip the LLM-judge call; only mechanical + workflow + audit"
    )]
    no_judge: bool,
    #[arg(long, help = "Skip the mechanical (cargo test) call")]
    no_mechanical: bool,
    #[arg(long, help = "Skip the diff-scope analyzer")]
    no_scope: bool,
    #[arg(long, help = "Skip the test-regression analyzer")]
    no_regression: bool,
    #[arg(long, help = "Skip the replay-based tool grader")]
    no_grade_tools: bool,

    #[arg(
        long,
        help = "Git ref representing pre-session state for diff-scope analysis (e.g. main, HEAD~5, abc123)"
    )]
    baseline_ref: Option<String>,
    #[arg(
        long,
        help = "Semicolon-separated globs of in-scope paths (e.g. 'src/**;.sovereign/features/oicp-core/**'). Default: '**/*' (everything except scorer/, runs/, .git/)."
    )]
    allowed_paths: Option<String>,

    #[arg(
        long,
        help = "Path to a baseline mechanical.json captured before the session; enables regression analysis"
    )]
    baseline_mechanical: Option<PathBuf>,

    #[arg(
        long,
        help = "Comma-separated explicit list of source files for the judge prompt (defaults: src/**/*.rs + Cargo.toml; or git-diff scope when --baseline-ref is set)"
    )]
    judge_files: Option<String>,
    #[arg(
        long,
        help = "Path to the authoritative spec for the judge to read (defaults: oicp-v0.3.md / spec.md / SPEC.md / PROTOCOL.md at the experiment-repo root)"
    )]
    contract_path: Option<PathBuf>,
    #[arg(long, help = "Max bytes of source the judge sees (default: 200KB)")]
    judge_max_bytes: Option<usize>,

    #[arg(
        long,
        default_value = "http://localhost:9741/mcp/message",
        help = "MCP message endpoint for tool-call replay"
    )]
    mcp_url: String,
}

fn cmd_finalize_run(
    data_dir: &Path,
    run_id: &str,
    experiment_repo: Option<&Path>,
    daemon_url: &str,
    out_dir: Option<&Path>,
    no_close: bool,
) -> Result<()> {
    let features_db = data_dir.join("features.db");
    let notes_db = data_dir.join("notes.db");

    if !no_close {
        let modified = finalize::close_run(&features_db, run_id)
            .with_context(|| format!("closing run `{run_id}`"))?;
        if modified {
            tracing::info!(run_id, "run closed");
        } else {
            tracing::info!(run_id, "run was already closed; re-dumping manifest");
        }
    }

    let daemon_url_opt = if daemon_url == "skip" {
        None
    } else {
        Some(daemon_url)
    };

    let m = manifest::build(manifest::BuildOpts {
        features_db: &features_db,
        notes_db: &notes_db,
        run_id,
        experiment_repo,
        daemon_url: daemon_url_opt,
    })
    .context("building manifest")?;

    let target_dir = run_dir(data_dir, out_dir, run_id);
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("mkdir {}", target_dir.display()))?;
    let manifest_path = target_dir.join("manifest.json");
    write_pretty_json(&manifest_path, &m).context("writing manifest")?;
    tracing::info!(
        path = %manifest_path.display(),
        tool_call_events = m.tool_calls.len(),
        decisions = m.notes.decisions.len(),
        invariants = m.notes.invariants.len(),
        uncertainties = m.notes.uncertainties.len(),
        deviations = m.notes.deviations.len(),
        spec_shas = m.experiment_repo.spec_shas.len(),
        "manifest written"
    );
    println!("{}", manifest_path.display());
    Ok(())
}

fn cmd_score_impl(data_dir: &Path, args: ScoreArgs) -> Result<()> {
    let target_dir = run_dir(data_dir, args.out_dir.as_deref(), &args.run_id);
    let manifest_path = target_dir.join("manifest.json");
    if !manifest_path.exists() {
        bail!(
            "manifest.json not found at {} — run finalize-run first",
            manifest_path.display()
        );
    }

    let m: manifest::Manifest = serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
        .context("parsing manifest")?;

    // Workflow always runs.
    let workflow_report = workflow::analyze(&m);
    write_pretty_json(&target_dir.join("workflow.json"), &workflow_report)?;
    tracing::info!(
        total = workflow_report.total_tool_calls,
        retries = workflow_report.retry_calls,
        elapsed = workflow_report.elapsed_seconds,
        "workflow analyzed"
    );

    // Mechanical
    let mech = if args.no_mechanical {
        None
    } else {
        let golden =
            mechanical::discover_golden_manifest(&args.experiment_repo).ok_or_else(|| {
                anyhow::anyhow!(
                    "scorer/golden/Cargo.toml not found under {}",
                    args.experiment_repo.display()
                )
            })?;
        let report = mechanical::run(&golden).context("mechanical scorer")?;
        write_pretty_json(&target_dir.join("mechanical.json"), &report)?;
        tracing::info!(
            passed = report.tests_passed,
            failed = report.tests_failed,
            total = report.tests_total,
            compile_failed = report.compile_failed,
            "mechanical scored"
        );
        Some(report)
    };

    let mech_pass = mech
        .as_ref()
        .map(|r| !r.compile_failed && r.tests_failed == 0 && r.tests_passed > 0)
        .unwrap_or(false);

    // Seam #3 — regression. Compare baseline vs. either the freshly-
    // computed `mech` or a pre-existing mechanical.json in the run dir.
    if !args.no_regression {
        if let Some(baseline_path) = &args.baseline_mechanical {
            if !baseline_path.exists() {
                bail!("baseline mechanical not found: {}", baseline_path.display());
            }
            let current_owned: Option<mechanical::MechanicalReport> = match &mech {
                Some(m) => Some(m.clone()),
                None => {
                    let existing = target_dir.join("mechanical.json");
                    if existing.exists() {
                        Some(
                            serde_json::from_str(&std::fs::read_to_string(&existing)?)
                                .context("parsing existing mechanical.json")?,
                        )
                    } else {
                        None
                    }
                }
            };
            if let Some(current) = current_owned {
                let baseline: mechanical::MechanicalReport =
                    serde_json::from_str(&std::fs::read_to_string(baseline_path)?)
                        .context("parsing baseline mechanical.json")?;
                let report = regression::compare_to_baseline(&baseline, &current);
                write_pretty_json(&target_dir.join("regression.json"), &report)?;
                tracing::info!(
                    regressions = report.regression_count,
                    fixes = report.fixes.len(),
                    new_passing = report.new_passing_tests,
                    "regressions analyzed"
                );
            } else {
                tracing::warn!(
                    "regression skipped: no mechanical.json in run dir and --no-mechanical was set"
                );
            }
        }
    }

    // Seam #1 — scope
    if !args.no_scope {
        if let Some(baseline) = &args.baseline_ref {
            let allowed = parse_allowed_paths(args.allowed_paths.as_deref());
            let report = scope::analyze(&args.experiment_repo, baseline, &allowed)
                .context("scope analyzer")?;
            write_pretty_json(&target_dir.join("scope.json"), &report)?;
            tracing::info!(
                in_scope = report.in_scope_changes.len(),
                out_of_scope = report.out_of_scope_changes.len(),
                compliance = report.scope_compliance,
                "scope analyzed"
            );
        }
    }

    // Seam #2 — replay-based tool grader
    if !args.no_grade_tools {
        if let Some(oracle_dir) = tool_grader::discover_oracle_dir(&args.experiment_repo) {
            let report = tool_grader::grade(tool_grader::GradeOpts {
                manifest: &m,
                oracle_dir: &oracle_dir,
                mcp_url: &args.mcp_url,
            })
            .context("tool grader")?;
            write_pretty_json(&target_dir.join("tool_grades.json"), &report)?;
            tracing::info!(
                graded = report.graded_calls,
                ungradeable = report.ungradeable_calls,
                replay_errors = report.replay_errors,
                "tool grades computed"
            );
        } else {
            tracing::info!("no oracle/symbols_oracle.json — tool grading skipped");
        }
    }

    // Seam #4 — judge with parameterized inputs
    if !args.no_judge {
        let feature_id = &m.run.feature_id;
        let source_files = args.judge_files.as_deref().map(parse_judge_files);
        let inputs = judge::read_inputs(judge::ReadOpts {
            experiment_repo: &args.experiment_repo,
            feature_id,
            contract_path: args.contract_path.as_deref(),
            source_files,
            baseline_ref: args.baseline_ref.as_deref(),
            max_bytes: args.judge_max_bytes,
        })
        .context("reading judge inputs")?;
        let agent_notes_summary = render_notes_summary(&m);
        let judge_report = judge::run(
            &args.daemon_url,
            judge::JudgeInputs {
                contract_label: &inputs.contract_label,
                spec_text: &inputs.spec_text,
                architecture_md: inputs.architecture_md.as_deref(),
                feature_spec_md: inputs.feature_spec_md.as_deref(),
                agent_notes_summary: &agent_notes_summary,
                agent_source: &inputs.agent_source,
                source_file_list: inputs.source_file_list.clone(),
                mechanical_pass: mech_pass,
            },
        )
        .context("judge scorer")?;
        write_pretty_json(&target_dir.join("judge_report.json"), &judge_report)?;
        tracing::info!(
            total = judge_report.total,
            spec_fidelity = judge_report.axes.spec_fidelity.score,
            files_in_prompt = judge_report.source_files_in_prompt.len(),
            bytes_in_prompt = judge_report.source_bytes_in_prompt,
            "judge scored"
        );
    }

    // Audit trail (requires --against)
    if let Some(other_run) = &args.against {
        let other_dir = run_dir(data_dir, None, other_run);
        let other_path = other_dir.join("manifest.json");
        if !other_path.exists() {
            bail!(
                "audit-trail comparison run not finalized: {} missing",
                other_path.display()
            );
        }
        let earlier: manifest::Manifest =
            serde_json::from_str(&std::fs::read_to_string(&other_path)?)?;
        let audit = audit_trail::analyze(&earlier, &m);
        write_pretty_json(&target_dir.join("audit_trail.json"), &audit)?;
        tracing::info!(
            coverage = audit.coverage,
            matched = audit.matched_notes,
            substantive = audit.run1_substantive_notes,
            "audit-trail analyzed"
        );
    }

    println!("scored: {}", target_dir.display());
    Ok(())
}

fn parse_allowed_paths(s: Option<&str>) -> Vec<String> {
    match s {
        Some(s) => s
            .split(';')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect(),
        None => scope::default_allowed_globs(),
    }
}

fn parse_judge_files(s: &str) -> Vec<PathBuf> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn cmd_diff(data_dir: &Path, run_a: &str, run_b: &str, out_dir: Option<&Path>) -> Result<()> {
    let a_dir = run_dir(data_dir, out_dir, run_a);
    let b_dir = run_dir(data_dir, out_dir, run_b);
    let a = diff_mod::load_bundle(&a_dir).with_context(|| format!("loading {run_a}"))?;
    let b = diff_mod::load_bundle(&b_dir).with_context(|| format!("loading {run_b}"))?;
    print!("{}", diff_mod::render(&a, &b));
    Ok(())
}

fn cmd_audit(
    data_dir: &Path,
    earlier_run: &str,
    later_run: &str,
    out_dir: Option<&Path>,
) -> Result<()> {
    let earlier_dir = run_dir(data_dir, out_dir, earlier_run);
    let later_dir = run_dir(data_dir, out_dir, later_run);
    let earlier: manifest::Manifest =
        serde_json::from_str(&std::fs::read_to_string(earlier_dir.join("manifest.json"))?)?;
    let later: manifest::Manifest =
        serde_json::from_str(&std::fs::read_to_string(later_dir.join("manifest.json"))?)?;
    let report = audit_trail::analyze(&earlier, &later);
    write_pretty_json(&later_dir.join("audit_trail.json"), &report)?;
    println!(
        "audit_trail: coverage={:.1}%  matched {}/{} substantive notes; {} `notes` queries in run {}",
        report.coverage * 100.0,
        report.matched_notes,
        report.run1_substantive_notes,
        report.run2_notes_queries,
        later_run,
    );
    Ok(())
}

fn render_notes_summary(m: &manifest::Manifest) -> String {
    let mut out = String::new();
    let mut push_section = |label: &str, ns: &[manifest::ManifestNote]| {
        out.push_str(&format!("{} ({}):\n", label, ns.len()));
        for n in ns {
            out.push_str(&format!("  - {}\n", n.content.lines().next().unwrap_or("")));
        }
    };
    push_section("decisions", &m.notes.decisions);
    push_section("invariants", &m.notes.invariants);
    push_section("uncertainties", &m.notes.uncertainties);
    push_section("attempts", &m.notes.attempts);
    if m.notes.decisions.is_empty()
        && m.notes.invariants.is_empty()
        && m.notes.uncertainties.is_empty()
        && m.notes.attempts.is_empty()
    {
        return "(no notes recorded)\n".to_string();
    }
    out
}

fn run_dir(data_dir: &Path, override_dir: Option<&Path>, run_id: &str) -> PathBuf {
    match override_dir {
        Some(p) => p.to_path_buf(),
        None => data_dir.join("runs").join(run_id),
    }
}

fn write_pretty_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(value).context("serializing")?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn resolve_data_dir(cli_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = cli_override {
        return Ok(p.to_path_buf());
    }
    let p = sovereign_contracts::rebrand::data_dir();
    if !p.exists() {
        bail!("data directory not found at {}", p.display());
    }
    Ok(p)
}
