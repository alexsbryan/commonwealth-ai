// SPDX-License-Identifier: AGPL-3.0-or-later
//! rec-1 ring 2: the local model behind the evaluator, over the daemon.
//! Never in CI — needs the daemon at `RECUR_DAEMON` (default
//! http://localhost:9741) with `RECUR_MODEL` resident (default the 4B fast
//! slot). Run explicitly:
//!
//!   cargo test -p sovereign-tdd --test main recur_ring2 -- --ignored --nocapture
//!
//! Bars (order `.sovereign/features/rec-1-explicit-stack`): prefix family
//! (1 LEARNED then HIT on every later ask), grammar (0 unparseable), restore
//! fidelity (kill bar), determinism (`RECUR_RUNS` identical event logs,
//! default 5). Cost is measured, not barred: the flat control arm is the
//! existing solve loop on the same fixture.

use crate::recur_fixture::{fixture, g, pytest_available, root_path, strip_paths};
use sovereign_tdd::recur::{
    driver::delivered_to, AskRecord, Driver, DriverConfig, Event, GoalCatalog, ModelConfig,
    ModelEvaluator, RECUR_MODEL_INSTRUCTION,
};
use sovereign_tdd::tasks::make_passing::{make_failing_tests_pass, MakePassingArgs};
use sovereign_tdd::{run_trial, ReqwestChatBackend, TrialConfig};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn model() -> String {
    std::env::var("RECUR_MODEL").unwrap_or_else(|_| "Qwopus3.5-4B-v3-MTP-Q8_0".into())
}

/// Ring 2 runs with `# BUG:` hints; ring 3 is the same harness with
/// `RECUR_HINTS=0`. The delta between the two is the model's contribution,
/// isolated — every other input is identical.
fn hints() -> bool {
    !matches!(
        std::env::var("RECUR_HINTS").as_deref(),
        Ok("0") | Ok("false")
    )
}

fn runs() -> usize {
    std::env::var("RECUR_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5)
}

fn daemon_err() -> PathBuf {
    std::env::var("RECUR_DAEMON_ERR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".sovereign/logs/daemon.err")
        })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `(learned, hit)` prefix-state events for `model` appended to daemon.err
/// after byte offset `from`.
fn prefix_events_since(from: u64, model: &str) -> Option<(usize, usize)> {
    let bytes = std::fs::read(daemon_err()).ok()?;
    let tail = String::from_utf8_lossy(&bytes[(from as usize).min(bytes.len())..]);
    let mut learned = 0;
    let mut hit = 0;
    for line in tail.lines() {
        let line = strip_ansi(line);
        if !line.contains("prefix_state") || !line.contains(model) {
            continue;
        }
        if line.contains("LEARNED") {
            learned += 1;
        } else if line.contains("HIT") {
            hit += 1;
        }
    }
    Some((learned, hit))
}

fn log_len() -> u64 {
    std::fs::metadata(daemon_err())
        .map(|m| m.len())
        .unwrap_or(0)
}

struct RunReport {
    root: String,
    rejected: usize,
    slots: Vec<String>,
    events: String,
    asks: Vec<AskRecord>,
    wall: Duration,
}

async fn one_run(fidelity: bool) -> RunReport {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let wd = fixture(&repo, hints());
    // The goal tree, from pytest's own collection, rooted at the directory
    // the run starts from. A node's parent is its file; a file's parent is
    // the root — which is what `split` may decompose into.
    let catalog = GoalCatalog::from_pytest(&repo)
        .unwrap()
        .under_root(g("tests"));
    assert!(
        catalog
            .goals()
            .iter()
            .any(|c| c.0 == "tests/test_h.py::test_base"),
        "{catalog:?}"
    );
    let mut mc = ModelConfig::local(model());
    mc.fidelity_probe = fidelity;
    // The cost counterfactual: RECUR_PIN=0 sends the same bytes with no
    // `stable_prefix_len`, so the instruction is re-prefilled every ask
    // instead of restored. Same prompts, same moves — only the engine's
    // work differs, which is the claim under test.
    mc.pin = !matches!(std::env::var("RECUR_PIN").as_deref(), Ok("0") | Ok("false"));
    let ev = ModelEvaluator::new(mc);
    let mut cfg = DriverConfig::pytest(tmp.path().join("scratch"));
    cfg.instruction = RECUR_MODEL_INSTRUCTION;
    cfg.catalog = catalog;
    let t0 = Instant::now();
    let mut d = Driver::start(&wd, g("tests"), cfg, ev).unwrap();
    let root = d.run().await.unwrap();
    let wall = t0.elapsed();
    let slots = delivered_to(d.state(), &root_path())
        .iter()
        .map(|v| format!("{}={} ({})", v.goal, v.verdict.as_str(), v.reason))
        .collect();
    let rejected = d
        .state()
        .events
        .iter()
        .filter(|e| matches!(e, Event::Rejected { .. }))
        .count();
    RunReport {
        root: format!("{} ({})", root.verdict.as_str(), root.reason),
        rejected,
        slots,
        events: strip_paths(d.state()),
        asks: d.evaluator().asks(),
        wall,
    }
}

fn cost_line(r: &RunReport) -> String {
    let p: u32 = r.asks.iter().map(|a| a.prompt_tokens).sum();
    let c: u32 = r.asks.iter().map(|a| a.completion_tokens).sum();
    let m: u128 = r.asks.iter().map(|a| a.wall_ms).sum();
    format!(
        "asks={} rejected_edits={} prompt_tokens={p} completion_tokens={c} model_ms={m} run_wall_s={:.1}",
        r.asks.len(),
        r.rejected,
        r.wall.as_secs_f64()
    )
}

/// Prefix family · grammar · flat frame · determinism · cost.
#[tokio::test]
#[ignore = "needs the daemon and a resident model"]
async fn ring2_model_run_meets_the_inference_bars() {
    assert!(pytest_available(), "python3 -m pytest unavailable");
    let n = runs();
    eprintln!(
        "ring {}: hints={} runs={n}",
        if hints() { 2 } else { 3 },
        hints()
    );
    let log_from = log_len();
    let mut reports = Vec::new();
    for i in 0..n {
        let r = one_run(false).await;
        eprintln!("run {i}: root {}", r.root);
        for s in &r.slots {
            eprintln!("  {s}");
        }
        eprintln!("  {}", cost_line(&r));
        for a in &r.asks {
            eprintln!(
                "    d{} {:>5}ms {}",
                a.depth,
                a.wall_ms,
                a.reply.lines().next().unwrap_or("")
            );
            if a.reply.starts_with("edit ") {
                for l in a.reply.lines().skip(1) {
                    eprintln!("        | {l}");
                }
            }
        }
        reports.push(r);
    }
    // BAR grammar: every reply parsed.
    let unparsed: Vec<&AskRecord> = reports
        .iter()
        .flat_map(|r| r.asks.iter())
        .filter(|a| !a.parsed)
        .collect();
    assert!(unparsed.is_empty(), "unparseable replies: {unparsed:#?}");
    // BAR flat frame: prompt bytes at depth >= 5 within 512 of depth <= 2.
    let all: Vec<&AskRecord> = reports.iter().flat_map(|r| r.asks.iter()).collect();
    let max_at = |pred: &dyn Fn(usize) -> bool| {
        all.iter()
            .filter(|a| pred(a.depth))
            .map(|a| a.prompt_bytes)
            .max()
            .unwrap_or(0)
    };
    let shallow = max_at(&|d| d <= 2);
    let deep = max_at(&|d| d >= 5);
    eprintln!("flat-frame: shallow={shallow} deep={deep} (deep may be 0 if the model never went that deep)");
    if deep > 0 {
        assert!(deep <= shallow + 512, "deep {deep} vs shallow {shallow}");
    }
    // BAR prefix family: one LEARNED for this model across all runs, and a
    // HIT for every ask after it.
    let total_asks = all.len();
    match prefix_events_since(log_from, &model()) {
        Some((learned, hit)) => {
            eprintln!("prefix-family: learned={learned} hit={hit} asks={total_asks}");
            // Cold cache: one LEARNED. Warm cache (an earlier run pinned the same
            // instruction): zero, and every ask is a HIT.
            assert!(
                learned <= 1,
                "one family, learned at most once; saw {learned}"
            );
            assert!(hit + 1 >= total_asks, "hit {hit} + 1 < asks {total_asks}");
        }
        None => panic!(
            "could-not-judge: daemon.err not readable at {}",
            daemon_err().display()
        ),
    }
    // BAR determinism: every run's event log is byte-identical.
    for (i, r) in reports.iter().enumerate().skip(1) {
        assert_eq!(reports[0].events, r.events, "run {i} diverged from run 0");
    }
}

/// Restore fidelity — the KILL bar. Every ask is a fresh pinned family sent
/// twice: Learn path, then Restore path. Outputs must agree, every time.
#[tokio::test]
#[ignore = "needs the daemon and a resident model"]
async fn ring2_restore_fidelity_is_exact() {
    assert!(pytest_available(), "python3 -m pytest unavailable");
    let r = one_run(true).await;
    eprintln!("hints={} root {}", hints(), r.root);
    eprintln!("{}", cost_line(&r));
    let checked: Vec<&AskRecord> = r.asks.iter().filter(|a| a.fidelity.is_some()).collect();
    let mismatched: Vec<&AskRecord> = checked
        .iter()
        .copied()
        .filter(|a| a.fidelity == Some(false))
        .collect();
    eprintln!(
        "fidelity: {} asks checked, {} mismatched",
        checked.len(),
        mismatched.len()
    );
    assert!(!checked.is_empty());
    assert!(
        mismatched.is_empty(),
        "restore is not a stack on this build: {mismatched:#?}"
    );
}

/// The flat control arm: the existing solve loop on the same fixture, one
/// candidate per round. Measured, not barred.
#[tokio::test]
#[ignore = "needs the daemon and a resident model"]
async fn ring2_flat_control_arm() {
    assert!(pytest_available(), "python3 -m pytest unavailable");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let wd = fixture(&repo, hints());
    let base = std::env::var("RECUR_DAEMON").unwrap_or_else(|_| "http://localhost:9741".into());
    let trial = make_failing_tests_pass(MakePassingArgs {
        workdir: wd,
        model: model(),
        task: None,
        test_command: Some(
            "PYTHONDONTWRITEBYTECODE=1 python3 -m pytest -q -p no:cacheprovider --continue-on-collection-errors tests".into(),
        ),
        config: Some(TrialConfig {
            candidates_per_round: 1,
            rounds_per_trial: 8,
            max_stall_rounds: 3,
            candidate_test_timeout: Duration::from_secs(60),
            ..TrialConfig::default()
        }),
    });
    let t0 = Instant::now();
    let result = run_trial(
        trial,
        Arc::new(ReqwestChatBackend::new(format!("{base}/v1"))),
    )
    .await;
    eprintln!(
        "flat control: status={:?} before={}/{} after={}/{} rounds={} wall_s={:.1}",
        result.status,
        result.tests_before.passed,
        result.tests_before.total,
        result.tests_after.passed,
        result.tests_after.total,
        result.rounds,
        t0.elapsed().as_secs_f64()
    );
}
