//! `sovereign search-gym …` — correctness harness for the
//! web-search-during-inference flow.
//!
//! The gym hits the running daemon at `/v1/chat/completions` with a
//! fixture's pre-baked `tools=[search,...]` request, observes the
//! model's tool_calls, resolves search invocations through a
//! deterministic `MockSearchProvider` (fixture-replay; never live),
//! feeds results back, and scores the final synthesis against the
//! fixture's `pass.toml` predicate.
//!
//! Two axes of correctness are pinned:
//!   1. **Judiciousness** — did the model invoke search at all,
//!      with a well-formed query, no more than necessary?
//!   2. **Faithful synthesis** — were the model's claims grounded in
//!      cited URLs from the mock response? No fabricated cites?
//!
//! Design + phasing: `sovereign-recipes/search-gym/RUNBOOK.md`.
//! Predicate vocabulary: `sovereign-recipes/search-gym/PASS_SCHEMA.md`.

// Phase-4 of the Tool-Mastery framework moved the judge surface to
// `crate::gym_judge`. We re-import under the local name `judge` so
// the rest of this module reads as before without per-call-site
// churn — see main.rs for the shared declaration.
use crate::gym_judge as judge;
mod judge_calibration;
mod predicate;
mod runner;
mod score;

#[cfg(test)]
pub use predicate::Predicate;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign search-gym",
    summary: "Correctness harness for web-search-during-inference. Replay fixtures \
              against the running daemon, score tool-call judiciousness and synthesis.",
    sections: &[
        HelpSection::Usage(
            "sovereign search-gym run [--fixture SLUG] [--replays N] [--base-url URL] \
             [--fixtures-dir PATH] [--mock-corpus PATH] [--json]",
        ),
        HelpSection::Subcommands(&[
            (
                "run",
                "Sweep fixtures, score each, print summary table (or JSON).",
            ),
            (
                "calibrate-judge",
                "Replay the hand-labeled calibration bank against the configured judge \
                 model; print per-category agreement + mint a CalibrationProof on a \
                 passing run. Required before judge verdicts are trustworthy.",
            ),
        ]),
        HelpSection::Flags(&[
            (
                "--fixture SLUG",
                "Run only the named fixture (directory name under fixtures/). \
                 Repeatable. Default: all fixtures.",
            ),
            (
                "--replays N",
                "Replays per fixture (default 10, matching the code gym).",
            ),
            (
                "--base-url URL",
                "Daemon base URL (default http://localhost:9741).",
            ),
            (
                "--fixtures-dir PATH",
                "Override the fixtures directory. Default: \
                 sovereign-recipes/search-gym/fixtures/",
            ),
            (
                "--mock-corpus PATH",
                "Override the mock-corpus directory. Default: \
                 sovereign-recipes/search-gym/mock-corpus/",
            ),
            (
                "--max-results N",
                "Cap on search results passed back to the model (default 5).",
            ),
            (
                "--mock",
                "Use the on-disk mock-corpus search backend (default). Deterministic; \
                 never spends API budget.",
            ),
            (
                "--synth",
                "Use the daemon's live search backend. Synth-mode plumbing is Phase 2g — \
                 the flag is parsed today but the runner bouncers out until 2g lands.",
            ),
            (
                "--no-judge",
                "Skip judge-evaluated semantic predicates (e.g. final_message_satisfies). \
                 Faster; useful while iterating on structural predicates. Without --no-judge \
                 the gym runs the FastInferenceJudge against commonwealth/fast.",
            ),
            (
                "--judge-model NAME",
                "Override the judge model id. Default: commonwealth/fast. Useful if you \
                 want to bench-compare judges against the calibration bank.",
            ),
            (
                "--json",
                "Emit machine-readable JSON instead of the human-readable table.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign search-gym run",
                "Replay every fixture 10× against the local daemon.",
            ),
            (
                "sovereign search-gym run --fixture 01_should_search_temporal_news --replays 3",
                "Iterate on a single fixture during fixture authoring.",
            ),
            (
                "sovereign search-gym run --json > run-$(date +%s).json",
                "Capture a machine-readable run for diffing across model changes.",
            ),
        ]),
        HelpSection::Notes(
            "The gym never calls a live search provider — only the on-disk mock corpus. \
             If a fixture's input induces the model to search for a query whose response \
             isn't in mock-corpus/, the fixture fails loudly with `mock search fixture \
             missing` rather than silently falling through. Record-mode for harvesting \
             real Tavily responses lives separately under `sovereign bench search-tavily` \
             (Phase 4 — not landed yet).",
        ),
    ],
};

pub async fn run_search_gym(args: &[String]) -> i32 {
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        help::print(&HELP);
        return 0;
    }

    match args[0].as_str() {
        "run" => match parse_run_args(&args[1..]) {
            Ok(opts) => execute_run(opts).await,
            Err(e) => {
                eprintln!("search-gym: {e}");
                help::print(&HELP);
                2
            }
        },
        "calibrate-judge" => match parse_calibrate_args(&args[1..]) {
            Ok(opts) => execute_calibrate(opts).await,
            Err(e) => {
                eprintln!("search-gym calibrate-judge: {e}");
                help::print(&HELP);
                2
            }
        },
        other => {
            eprintln!("search-gym: unknown subcommand {other:?}");
            help::print(&HELP);
            2
        }
    }
}

#[derive(Debug)]
struct RunOpts {
    fixtures: Vec<String>,
    replays: usize,
    base_url: String,
    fixtures_dir: PathBuf,
    mock_corpus: PathBuf,
    max_results: usize,
    mode: runner::Mode,
    use_judge: bool,
    judge_model: String,
    json: bool,
}

fn parse_run_args(args: &[String]) -> Result<RunOpts, String> {
    // Workspace-root anchors so the gym works from any subdir.
    let workspace_root = workspace_root();
    let mut opts = RunOpts {
        fixtures: Vec::new(),
        replays: 10,
        base_url: "http://localhost:9741".to_string(),
        fixtures_dir: workspace_root.join("sovereign-recipes/search-gym/fixtures"),
        mock_corpus: workspace_root.join("sovereign-recipes/search-gym/mock-corpus"),
        max_results: 5,
        mode: runner::Mode::Mock,
        use_judge: true,
        judge_model: "commonwealth/fast".to_string(),
        json: false,
    };

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--fixture" => {
                let v = args.get(i + 1).ok_or("--fixture requires a value")?;
                opts.fixtures.push(v.clone());
                i += 2;
            }
            "--replays" => {
                let v = args.get(i + 1).ok_or("--replays requires a value")?;
                opts.replays = v.parse().map_err(|e| format!("--replays {v}: {e}"))?;
                i += 2;
            }
            "--base-url" => {
                opts.base_url = args
                    .get(i + 1)
                    .ok_or("--base-url requires a value")?
                    .clone();
                i += 2;
            }
            "--fixtures-dir" => {
                opts.fixtures_dir = args
                    .get(i + 1)
                    .ok_or("--fixtures-dir requires a value")?
                    .into();
                i += 2;
            }
            "--mock-corpus" => {
                opts.mock_corpus = args
                    .get(i + 1)
                    .ok_or("--mock-corpus requires a value")?
                    .into();
                i += 2;
            }
            "--max-results" => {
                let v = args.get(i + 1).ok_or("--max-results requires a value")?;
                opts.max_results = v.parse().map_err(|e| format!("--max-results {v}: {e}"))?;
                i += 2;
            }
            "--mock" => {
                opts.mode = runner::Mode::Mock;
                i += 1;
            }
            "--synth" => {
                opts.mode = runner::Mode::Synth;
                i += 1;
            }
            "--no-judge" => {
                opts.use_judge = false;
                i += 1;
            }
            "--judge-model" => {
                opts.judge_model = args
                    .get(i + 1)
                    .ok_or("--judge-model requires a value")?
                    .clone();
                i += 2;
            }
            "--json" => {
                opts.json = true;
                i += 1;
            }
            other => return Err(format!("unknown flag: {other:?}")),
        }
    }

    Ok(opts)
}

async fn execute_run(opts: RunOpts) -> i32 {
    let fixtures = match discover_fixtures(&opts.fixtures_dir, &opts.fixtures) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("search-gym: {e}");
            return 2;
        }
    };
    if fixtures.is_empty() {
        eprintln!(
            "search-gym: no fixtures found under {}",
            opts.fixtures_dir.display()
        );
        return 2;
    }

    if !opts.mock_corpus.exists() {
        eprintln!(
            "search-gym: mock-corpus dir not found: {}",
            opts.mock_corpus.display()
        );
        return 2;
    }

    if !daemon_reachable(&opts.base_url).await {
        eprintln!(
            "search-gym: daemon not reachable at {} — start it with `sovereign daemon run`",
            opts.base_url
        );
        return 2;
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(200))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("search-gym: building http client: {e}");
            return 2;
        }
    };

    let cfg = runner::RunnerCfg {
        mode: opts.mode,
        base_url: opts.base_url.clone(),
        mock_corpus: opts.mock_corpus.clone(),
        max_search_results: opts.max_results,
    };

    // Build the judge unless --no-judge. Until Phase 2b lands a
    // calibration receipt, we mint an `untrusted()` receipt — every
    // judge call will warn that agreement with humans is unverified.
    let judge: Option<Box<dyn judge::Judge>> = if opts.use_judge {
        let cfg_j = judge::FastInferenceJudgeCfg {
            base_url: opts.base_url.clone(),
            model: opts.judge_model.clone(),
            ..Default::default()
        };
        Some(Box::new(judge::FastInferenceJudge::new(
            client.clone(),
            cfg_j,
            judge::CalibrationReceipt::untrusted(),
        )))
    } else {
        None
    };

    if !opts.json {
        eprintln!(
            "── search-gym ───────────────────────────────────────\n\
             daemon: {}\n\
             mode: {}\n\
             judge: {}\n\
             fixtures: {} (under {})\n\
             replays per fixture: {}\n\
             mock-corpus: {}\n",
            opts.base_url,
            opts.mode.id(),
            if opts.use_judge {
                format!("{} (uncalibrated — Phase 2b pending)", opts.judge_model)
            } else {
                "disabled (--no-judge)".to_string()
            },
            fixtures.len(),
            opts.fixtures_dir.display(),
            opts.replays,
            opts.mock_corpus.display(),
        );
    }

    let mut aggregates = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        let mut scored_replays = Vec::with_capacity(opts.replays);
        for _ in 0..opts.replays {
            let tx = runner::run_fixture(&client, &cfg, fixture).await;
            let scored = match &judge {
                Some(j) => {
                    score::score_with_judge(&fixture.slug, &fixture.predicate, &tx, j.as_ref())
                        .await
                }
                None => score::score(&fixture.slug, &fixture.predicate, &tx),
            };
            scored_replays.push(scored);
        }
        let agg = score::aggregate(&fixture.slug, &scored_replays);
        if !opts.json {
            eprintln!(
                "  {:<40} {}/{} ({:.0}%) {}",
                agg.slug,
                agg.passes,
                agg.replays,
                agg.rate * 100.0,
                agg.failure_reasons
                    .first()
                    .map(|r| format!("— {r}"))
                    .unwrap_or_default()
            );
        }
        aggregates.push(agg);
    }

    if opts.json {
        let total_pass: usize = aggregates.iter().map(|a| a.passes).sum();
        let total_run: usize = aggregates.iter().map(|a| a.replays).sum();
        let payload = serde_json::json!({
            "fixtures": aggregates,
            "total_pass": total_pass,
            "total_run": total_run,
            "total_rate": if total_run > 0 {
                total_pass as f32 / total_run as f32
            } else { 0.0 },
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("\n{}", score::render_table(&aggregates));
    }

    let any_failure = aggregates.iter().any(|a| a.passes < a.replays);
    if any_failure { 1 } else { 0 }
}

fn discover_fixtures(
    dir: &Path,
    filter: &[String],
) -> Result<Vec<runner::Fixture>, String> {
    if !dir.exists() {
        return Err(format!("fixtures dir not found: {}", dir.display()));
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("read_dir {}: {e}", dir.display()))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = entry.path();
        let slug = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !filter.is_empty() && !filter.iter().any(|f| f == &slug) {
            continue;
        }
        out.push(runner::Fixture::load(&path)?);
    }
    Ok(out)
}

async fn daemon_reachable(base_url: &str) -> bool {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[derive(Debug)]
struct CalibrateOpts {
    base_url: String,
    judge_model: String,
    cases_path: PathBuf,
    json: bool,
}

fn parse_calibrate_args(args: &[String]) -> Result<CalibrateOpts, String> {
    let workspace_root = workspace_root();
    let mut opts = CalibrateOpts {
        base_url: "http://localhost:9741".to_string(),
        judge_model: "commonwealth/fast".to_string(),
        cases_path: workspace_root
            .join("sovereign-recipes/search-gym/judge-calibration/cases.toml"),
        json: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--base-url" => {
                opts.base_url = args
                    .get(i + 1)
                    .ok_or("--base-url requires a value")?
                    .clone();
                i += 2;
            }
            "--judge-model" => {
                opts.judge_model = args
                    .get(i + 1)
                    .ok_or("--judge-model requires a value")?
                    .clone();
                i += 2;
            }
            "--cases" => {
                opts.cases_path = args
                    .get(i + 1)
                    .ok_or("--cases requires a value")?
                    .into();
                i += 2;
            }
            "--json" => {
                opts.json = true;
                i += 1;
            }
            other => return Err(format!("unknown flag: {other:?}")),
        }
    }
    Ok(opts)
}

async fn execute_calibrate(opts: CalibrateOpts) -> i32 {
    let bank = match judge_calibration::CaseBank::load(&opts.cases_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("search-gym calibrate-judge: {e}");
            return 2;
        }
    };
    if !daemon_reachable(&opts.base_url).await {
        eprintln!(
            "search-gym calibrate-judge: daemon not reachable at {} — start it with `sovereign daemon run`",
            opts.base_url
        );
        return 2;
    }
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(200))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("search-gym calibrate-judge: building http client: {e}");
            return 2;
        }
    };
    let judge_cfg = judge::FastInferenceJudgeCfg {
        base_url: opts.base_url.clone(),
        model: opts.judge_model.clone(),
        ..Default::default()
    };
    let j = judge::FastInferenceJudge::new(
        client,
        judge_cfg,
        // Calibration runs intrinsically use an uncalibrated judge —
        // the whole point is to test whether it deserves trust. The
        // warn! on every call is informative here, not noisy.
        judge::CalibrationReceipt::untrusted(),
    );

    if !opts.json {
        eprintln!(
            "── search-gym calibrate-judge ───────────────────────\n\
             daemon: {}\n\
             judge:  {}\n\
             cases:  {} ({} total)\n",
            opts.base_url,
            opts.judge_model,
            opts.cases_path.display(),
            bank.cases().len(),
        );
    }

    let (result, proof) = match judge_calibration::calibrate(&j, &bank, &opts.judge_model).await
    {
        Ok(tuple) => tuple,
        Err(e) => {
            eprintln!("search-gym calibrate-judge: {e}");
            return 2;
        }
    };

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        );
    } else {
        println!("{}", judge_calibration::render_report(&result));
    }

    // The proof itself is consumed here — Sprint 2 surface doesn't
    // yet persist it (that's wired into `run` in Sprint 3 once the
    // structural-only predicates retire). For now, a passing
    // calibration is signalled by exit code 0; future code wanting
    // a receipt re-runs calibration and uses the proof immediately.
    drop(proof);

    if result.overall_pass {
        0
    } else {
        1
    }
}

fn workspace_root() -> PathBuf {
    // The CLI binary lives at <root>/target/release/sovereign-cli post-
    // monorepo. Walk up from CARGO_MANIFEST_DIR at compile time, then
    // at runtime prefer the current working directory if it contains
    // a `sovereign-recipes` dir (which means the user invoked us from
    // the workspace root and that's the most reliable anchor).
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("sovereign-recipes").is_dir() {
            return cwd;
        }
    }
    // Fallback: hard-coded relative to the CLI crate. Two-deep
    // (sovereign/crates/sovereign-cli/) → workspace root is `../../..`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}
