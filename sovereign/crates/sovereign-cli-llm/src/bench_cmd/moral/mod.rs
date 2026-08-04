// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench moral` — process-focused moral-reasoning lane.
//!
//! Scores HOW a model reasons about moral dilemmas, not which
//! verdict it reaches: per-criterion binary judging against the
//! MoReBench-derived rubric bank under `bench/moral/scenarios/`
//! (identifying moral factors / logical process / clear process /
//! helpful outcome / harmless outcome), weighted aggregation per the
//! reference formula, per-dimension fulfillment rates.
//!
//! The intended workflow is a judge-pinned A/B:
//!
//! ```text
//! svrn bench moral --calibrate --judge-model <J>          # once per judge
//! svrn bench moral --all --chat-model A --judge-model <J> --report a.json
//! svrn bench moral --all --chat-model B --judge-model <J> --report b.json --diff a.json
//! ```
//!
//! Exit codes: 0 clean; 1 degraded run or failed calibration;
//! 2 usage; 4 nothing was actually judged (a zero-criteria run can
//! never be green — ARCH_PRINCIPLES §18.1).

mod judge;
mod report;
mod runner;
mod scenarios;

use std::path::PathBuf;

const BOOLEAN_FLAGS: &[&str] = &["all", "json", "help", "h", "calibrate"];

fn split_args(args: &[String]) -> Vec<(String, String)> {
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(name) = arg.strip_prefix("--") {
            if BOOLEAN_FLAGS.contains(&name) {
                flags.push((name.to_string(), String::new()));
                i += 1;
            } else {
                let value = args.get(i + 1).cloned().unwrap_or_default();
                flags.push((name.to_string(), value));
                i += 2;
            }
        } else {
            i += 1;
        }
    }
    flags
}

fn get_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    flags
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

fn has_flag(flags: &[(String, String)], name: &str) -> bool {
    flags.iter().any(|(k, _)| k == name)
}

pub async fn cmd_moral(args: &[String]) -> i32 {
    let flags = split_args(args);
    if has_flag(&flags, "help") || has_flag(&flags, "h") {
        print_help();
        return 0;
    }

    let daemon_base =
        get_flag(&flags, "daemon").unwrap_or_else(|| "http://localhost:9741".to_string());
    let judge_model = get_flag(&flags, "judge-model").unwrap_or_else(|| "primary".to_string());
    let judge_trials: u8 = match get_flag(&flags, "judge-trials").map(|v| v.parse()) {
        None => 1,
        Some(Ok(n)) => n,
        Some(Err(_)) => {
            eprintln!("bench moral: --judge-trials must be an integer");
            return 2;
        }
    };

    // ── Calibration mode ────────────────────────────────────────────
    if has_flag(&flags, "calibrate") {
        return run_calibrate(&flags, &daemon_base, &judge_model, judge_trials).await;
    }

    // ── Scoring mode ────────────────────────────────────────────────
    let scenarios_dir =
        match scenarios::resolve_scenarios_dir(get_flag(&flags, "scenarios-dir").as_deref()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("bench moral: {e}");
                return 2;
            }
        };
    let mut selected = match scenarios::load_all(&scenarios_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bench moral: {e}");
            return 2;
        }
    };
    if let Some(id) = get_flag(&flags, "scenario") {
        selected.retain(|s| s.scenario.id == id);
        if selected.is_empty() {
            eprintln!("bench moral: scenario `{id}` not found in {}", scenarios_dir.display());
            return 2;
        }
    }
    if let Some(limit) = get_flag(&flags, "limit") {
        match limit.parse::<usize>() {
            Ok(n) => selected.truncate(n),
            Err(_) => {
                eprintln!("bench moral: --limit must be an integer");
                return 2;
            }
        }
    }
    if selected.is_empty() {
        eprintln!(
            "bench moral: 0 scenarios selected from {} — nothing to judge",
            scenarios_dir.display()
        );
        return 4;
    }

    let opts = runner::RunOptions {
        daemon_base,
        chat_model: get_flag(&flags, "chat-model").unwrap_or_else(|| "primary".to_string()),
        judge_model,
        judge_trials,
        max_tokens: match get_flag(&flags, "max-tokens").map(|v| v.parse()) {
            None => 2000,
            Some(Ok(n)) => n,
            Some(Err(_)) => {
                eprintln!("bench moral: --max-tokens must be an integer");
                return 2;
            }
        },
    };

    let run = match runner::run(&selected, &opts).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bench moral: run failed: {e}");
            eprintln!(
                "Hint: is the daemon up (`svrn daemon start`) and are the model ids valid \
                 (`curl {}/v1/models`)?",
                opts.daemon_base
            );
            return 1;
        }
    };

    if let Some(path) = get_flag(&flags, "report").map(PathBuf::from) {
        if let Err(e) = report::write_json_report(&path, &run) {
            eprintln!("bench moral: failed to write report to {}: {e}", path.display());
            return 1;
        }
        eprintln!("bench moral: report written to {}", path.display());
    }

    if !has_flag(&flags, "json") {
        report::print_text_report(&run);
    }

    if let Some(diff_path) = get_flag(&flags, "diff").map(PathBuf::from) {
        match report::load_report(&diff_path) {
            Ok(baseline) => report::print_diff(&baseline, &run),
            Err(e) => {
                eprintln!("bench moral: failed to load baseline {}: {e}", diff_path.display());
                return 1;
            }
        }
    }

    // A run where nothing got judged proves nothing.
    let judged = run.aggregate.criteria_total - run.aggregate.could_not_judge;
    if judged == 0 {
        eprintln!("bench moral: zero criteria judged — not a result");
        return 4;
    }
    if run.aggregate.degraded {
        return 1;
    }
    0
}

async fn run_calibrate(
    flags: &[(String, String)],
    daemon_base: &str,
    judge_model: &str,
    judge_trials: u8,
) -> i32 {
    let cal_path = match get_flag(flags, "calibration-file") {
        Some(p) => PathBuf::from(p),
        None => {
            // calibration.toml sits next to scenarios/.
            match scenarios::resolve_scenarios_dir(get_flag(flags, "scenarios-dir").as_deref()) {
                Ok(d) => d.parent().map(|p| p.join("calibration.toml")).unwrap_or_default(),
                Err(e) => {
                    eprintln!("bench moral: {e}");
                    return 2;
                }
            }
        }
    };
    let bank = match judge::load_calibration(&cal_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("bench moral: calibration bank {}: {e}", cal_path.display());
            return 2;
        }
    };
    eprintln!(
        "bench moral: calibrating judge `{judge_model}` on {} labeled items (trials {})",
        bank.items.len(),
        judge_trials.max(1)
    );
    let v1 = format!("{}/v1", daemon_base.trim_end_matches('/'));
    let provider =
        sovereign_inference::remote::RemoteApiProvider::new(&v1, None, judge_model, 16384);
    let rep = judge::run_calibration(&provider, &bank, Some(judge_model), judge_trials).await;
    println!("Calibration — judge `{judge_model}`");
    println!("  items            {}", rep.items);
    println!(
        "  sensitivity      {:.3}  (floor {})",
        rep.sensitivity,
        judge::CALIBRATION_SENSITIVITY_FLOOR
    );
    println!(
        "  specificity      {:.3}  (floor {})",
        rep.specificity,
        judge::CALIBRATION_SPECIFICITY_FLOOR
    );
    println!(
        "  confusion        tp {} / fn {} / tn {} / fp {}  (could-not-judge {})",
        rep.true_pos, rep.false_neg, rep.true_neg, rep.false_pos, rep.could_not_judge
    );
    for m in &rep.misses {
        println!("  miss: {m}");
    }
    if rep.passed {
        println!("  PASSED — this judge's scores are comparable under the rubric");
        0
    } else {
        println!("  FAILED — do not compare scores produced by this judge");
        1
    }
}

fn print_help() {
    eprintln!("svrn bench moral — process-focused moral-reasoning lane (MoReBench-derived)");
    eprintln!();
    eprintln!("USAGE");
    eprintln!("  svrn bench moral [--all] [--scenario <id>] [--limit N]");
    eprintln!("                   [--chat-model <id>] [--judge-model <id>] [--judge-trials N]");
    eprintln!("                   [--report <path>] [--diff <baseline.json>] [--json]");
    eprintln!("  svrn bench moral --calibrate [--judge-model <id>] [--calibration-file <path>]");
    eprintln!();
    eprintln!("FLAGS");
    eprintln!("  --scenario <id>          Run one scenario (default: all).");
    eprintln!("  --limit N                Run only the first N scenarios (id order).");
    eprintln!("  --scenarios-dir <path>   Override the default bench/moral/scenarios location.");
    eprintln!("  --chat-model <id>        Model under test (default: daemon `primary` alias).");
    eprintln!("  --judge-model <id>       Judge model (default: `primary`). PIN THIS and keep it");
    eprintln!("                           identical across runs you intend to compare.");
    eprintln!("  --judge-trials N         Majority vote over N judge calls per criterion (default 1).");
    eprintln!("  --max-tokens N           Generation budget for the dilemma response (default 2000).");
    eprintln!("  --report <path>          Write the full JSON report (per-criterion verdicts + evidence).");
    eprintln!("  --diff <baseline.json>   Print per-dimension deltas vs a stored report.");
    eprintln!("  --json                   Suppress the text report.");
    eprintln!("  --daemon <url>           Daemon base URL (default http://localhost:9741).");
    eprintln!("  --calibrate              Score the judge itself against the hand-labeled bank;");
    eprintln!("                           gates on sensitivity/specificity >= 0.85.");
    eprintln!();
    eprintln!("SCORING");
    eprintln!("  Per criterion: yes/no + evidence quote. Per scenario: 100 * achieved / max over");
    eprintln!("  signed weights (see bench/moral/README.md). Failed judge calls are could-not-judge:");
    eprintln!("  counted, reported, never defaulted; >10% degrades the run (exit 1).");
}
