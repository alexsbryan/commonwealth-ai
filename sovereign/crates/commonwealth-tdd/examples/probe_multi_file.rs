// SPDX-License-Identifier: AGPL-3.0-or-later
//! Quick driver to probe the unified solver via `tasks::split_file`
//! against a real local daemon.
//!
//! Usage:
//!
//!     cargo run --release --example probe_multi_file -- \
//!         --workdir /tmp/tdd-multifile-probe \
//!         --path calc.py \
//!         --max-lines 30 \
//!         --test-command "pytest -q tests/" \
//!         --model commonwealth/primary \
//!         --provider-url http://localhost:9741/v1
//!
//! Prints the trajectory + tests-before/after.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_tdd::tasks::framework::{detect_framework, Framework};
use commonwealth_tdd::tasks::split_file::{cleanup_structural_test, SplitFileArgs};
use commonwealth_tdd::{run_trial, tasks::split_file, ReqwestChatBackend, TrialConfig, Workdir};

#[derive(Default)]
struct Args {
    workdir: Option<PathBuf>,
    path: Option<String>,
    max_lines: usize,
    test_command: Option<String>,
    model: String,
    provider_url: String,
    force: bool,
    cleanup: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        max_lines: 30,
        model: "commonwealth/primary".into(),
        provider_url: "http://localhost:9741/v1".into(),
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--workdir" => args.workdir = Some(it.next().expect("--workdir value").into()),
            "--path" => args.path = Some(it.next().expect("--path value")),
            "--max-lines" => {
                args.max_lines = it
                    .next()
                    .expect("--max-lines value")
                    .parse()
                    .expect("usize")
            }
            "--test-command" => args.test_command = Some(it.next().expect("--test-command value")),
            "--model" => args.model = it.next().expect("--model value"),
            "--provider-url" => args.provider_url = it.next().expect("--provider-url value"),
            "--force" => args.force = true,
            "--cleanup" => args.cleanup = true,
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
                tracing_subscriber::EnvFilter::new("commonwealth_tdd=info,probe_multi_file=info")
            }),
        )
        .init();

    let args = parse_args();
    let workdir_path = args.workdir.expect("--workdir required");
    let path = args.path.expect("--path required");

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
    config.rounds_per_trial = 2;
    config.max_stall_rounds = 2;
    config.candidate_test_timeout = Duration::from_secs(60);

    let trial = split_file(SplitFileArgs {
        workdir,
        model: args.model.clone(),
        path: path.clone().into(),
        max_lines: args.max_lines,
        test_command: args.test_command.clone(),
        config: Some(config),
    });

    println!("─── Probe ───────────────────────────────────────────");
    println!("workdir:         {}", workdir_path.display());
    println!("path:            {path}");
    println!("max_lines:       {}", args.max_lines);
    println!("model:           {}", args.model);
    println!("provider_url:    {}", args.provider_url);
    println!();

    let started = std::time::Instant::now();
    let r = run_trial(trial, backend).await;
    let elapsed = started.elapsed();

    println!("─── Result ──────────────────────────────────────────");
    println!("status:       {:?}", r.status);
    println!(
        "tests:        before={}p/{}f after={}p/{}f",
        r.tests_before.passed, r.tests_before.failed, r.tests_after.passed, r.tests_after.failed
    );
    println!("rounds:       {}", r.rounds);
    println!("wall time:    {:.1}s", elapsed.as_secs_f64());
    println!();
    println!("─── Trajectory ──────────────────────────────────────");
    for g in &r.trajectory {
        println!(
            "  round {}: winner={:?} passing_after={} failed_after={}",
            g.round, g.winner, g.passing_after, g.failed_after
        );
        for c in &g.candidates {
            println!("    cand: {c}");
        }
    }

    if args.cleanup {
        let framework = detect_framework(&workdir_path);
        cleanup_structural_test(&workdir_path, framework);
        println!("\nCleaned up generated structural test.");
        // The Framework import is held live via this path even when
        // the user doesn't pass --cleanup, so the symbol resolves
        // for the side-import.
        let _ = Framework::Pytest;
    }
}
