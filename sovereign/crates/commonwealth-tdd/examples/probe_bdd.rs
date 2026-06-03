//! Probe the bdd_cycle composition against a real daemon.
//!
//! Usage:
//!
//!     cargo run --release --example probe_bdd -- \
//!         --workdir /tmp/bdd-probe \
//!         --intent "evaluate('1 + 2 * 3') returns 7" \
//!         --test-file-hint tests/test_evaluate.py \
//!         --test-command "pytest -q tests/" \
//!         --model commonwealth/primary
//!
//! Prints synthesis result, generated test diff, and green result.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_tdd::tasks::bdd::{bdd_cycle, BddCycleArgs, ReviewMode};
use commonwealth_tdd::{ReqwestChatBackend, TrialConfig, Workdir};

#[derive(Default)]
struct Args {
    workdir: Option<PathBuf>,
    intent: Option<String>,
    test_file_hint: Option<String>,
    test_command: Option<String>,
    model: String,
    provider_url: String,
    force: bool,
    pause: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        model: "commonwealth/primary".into(),
        provider_url: "http://localhost:9741/v1".into(),
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--workdir" => args.workdir = Some(it.next().expect("--workdir value").into()),
            "--intent" => args.intent = Some(it.next().expect("--intent value")),
            "--test-file-hint" => {
                args.test_file_hint = Some(it.next().expect("--test-file-hint value"))
            }
            "--test-command" => args.test_command = Some(it.next().expect("--test-command value")),
            "--model" => args.model = it.next().expect("--model value"),
            "--provider-url" => args.provider_url = it.next().expect("--provider-url value"),
            "--force" => args.force = true,
            "--pause" => args.pause = true,
            other => panic!("unknown flag: {other}"),
        }
    }
    args
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("commonwealth_tdd=info,probe_bdd=info")
            }),
        )
        .init();

    let args = parse_args();
    let workdir_path = args.workdir.expect("--workdir required");
    let intent = args.intent.expect("--intent required");

    let workdir = match Workdir::check_safe(workdir_path.clone(), args.force) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("workdir refused: {e}");
            std::process::exit(2);
        }
    };

    let backend = Arc::new(ReqwestChatBackend::new(args.provider_url.clone()));

    let mut config = TrialConfig::default();
    config.candidates_per_round = 2;
    config.rounds_per_trial = 3;
    config.max_stall_rounds = 2;
    config.candidate_test_timeout = Duration::from_secs(60);

    println!("─── Probe (bdd_cycle) ──────────────────────────────");
    println!("workdir:        {}", workdir_path.display());
    println!("intent:         {intent}");
    println!("test_file_hint: {:?}", args.test_file_hint);
    println!("test_command:   {:?}", args.test_command);
    println!("model:          {}", args.model);
    println!(
        "review_mode:    {}",
        if args.pause {
            "PauseAfterSynthesis"
        } else {
            "Auto"
        }
    );
    println!();

    let started = std::time::Instant::now();
    let r = bdd_cycle(
        BddCycleArgs {
            workdir,
            model: args.model.clone(),
            intent,
            test_file_hint: args.test_file_hint.clone(),
            task_hint: None,
            test_command: args.test_command.clone(),
            config: Some(config),
            review_mode: if args.pause {
                ReviewMode::PauseAfterSynthesis
            } else {
                ReviewMode::Auto
            },
        },
        backend,
    )
    .await;
    let elapsed = started.elapsed();

    println!("─── Synthesis stage ────────────────────────────────");
    println!("status:        {:?}", r.synthesis.status);
    println!(
        "tests:         before={}p/{}f after={}p/{}f",
        r.synthesis.tests_before.passed,
        r.synthesis.tests_before.failed,
        r.synthesis.tests_after.passed,
        r.synthesis.tests_after.failed
    );
    println!("rounds:        {}", r.synthesis.rounds);
    println!("─── Synthesis trajectory ───────────────────────────");
    for g in &r.synthesis.trajectory {
        println!(
            "  round {}: winner={:?} passing={} failed={}",
            g.round, g.winner, g.passing_after, g.failed_after
        );
        for c in &g.candidates {
            println!("    cand: {c}");
        }
    }
    if let Some(p) = &r.generated_test_path {
        println!("test_path:     {}", p.display());
    }
    if let Some(c) = &r.generated_test_content {
        println!("─── Generated test ─────────────────────────────────");
        for line in c.lines().take(40) {
            println!("  {line}");
        }
        if c.lines().count() > 40 {
            println!("  ... ({} more lines)", c.lines().count() - 40);
        }
    }
    println!();
    if let Some(green) = r.green {
        println!("─── Green stage ────────────────────────────────────");
        println!("status:        {:?}", green.status);
        println!(
            "tests:         before={}p/{}f after={}p/{}f",
            green.tests_before.passed,
            green.tests_before.failed,
            green.tests_after.passed,
            green.tests_after.failed
        );
        println!("rounds:        {}", green.rounds);
        println!("─── Green trajectory ───────────────────────────────");
        for g in &green.trajectory {
            println!(
                "  round {}: winner={:?} passing={}",
                g.round, g.winner, g.passing_after
            );
        }
    } else {
        println!("─── Green stage skipped ────────────────────────────");
    }
    println!();
    println!("Total wall: {:.1}s", elapsed.as_secs_f64());
}
