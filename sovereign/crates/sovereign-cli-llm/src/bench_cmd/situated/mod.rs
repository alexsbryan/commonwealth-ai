// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench situated` — the situatedness process lane.
//!
//! The chaos banks grade situated *outcomes* (answered / abstained / leaked,
//! with a causal partition). This lane adds the *process* layer: WHICH
//! situated behaviour failed, per probe, per model — the layer a harness
//! change can act on (`SITUATED_FLYWHEEL.md`).
//!
//! It scores transcripts the chaos bench already produced through the
//! production turn; it never generates. See [`transcripts`] for why that is
//! the mandate rather than a convenience.
//!
//! ```text
//! svrn bench chaos-monkey ... --transcripts run.jsonl       # produce (production path)
//! svrn bench situated --calibrate --judge-model <J>          # once per judge, per vocabulary
//! svrn bench situated --transcripts run.jsonl --judge-model <J> --report a.json
//! svrn bench situated --transcripts run2.jsonl --judge-model <J> --diff a.json
//! ```
//!
//! Exit codes: 0 clean; 1 degraded run or failed calibration; 2 usage;
//! 4 nothing was actually judged (a zero-criteria run can never be green —
//! ARCH_PRINCIPLES §18.1).

mod criteria;
mod report;
mod runner;
mod transcripts;

use std::path::PathBuf;

use crate::bench_cmd::rubric::judge;

const BOOLEAN_FLAGS: &[&str] = &["json", "help", "h", "calibrate"];

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

pub async fn cmd_situated(args: &[String]) -> i32 {
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
            eprintln!("bench situated: --judge-trials must be an integer");
            return 2;
        }
    };

    let criteria_path = match criteria::resolve_criteria_path(get_flag(&flags, "criteria").as_deref())
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bench situated: {e}");
            return 2;
        }
    };
    let vocab = match criteria::load(&criteria_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bench situated: criteria {}: {e}", criteria_path.display());
            return 2;
        }
    };

    if has_flag(&flags, "calibrate") {
        return run_calibrate(&flags, &criteria_path, &vocab, &daemon_base, &judge_model, judge_trials)
            .await;
    }

    // ── Scoring mode ────────────────────────────────────────────────
    let Some(tpath) = get_flag(&flags, "transcripts") else {
        eprintln!(
            "bench situated: --transcripts <chaos-run.transcripts.jsonl> is required.\n\
             This lane scores what the production turn produced; it does not generate.\n\
             Produce transcripts first with `svrn bench chaos-monkey --transcripts <path>`."
        );
        return 2;
    };
    let tpath = PathBuf::from(tpath);
    let (mut rows, skipped) = match transcripts::load(&tpath) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bench situated: {e}");
            return 2;
        }
    };
    if let Some(limit) = get_flag(&flags, "limit") {
        match limit.parse::<usize>() {
            Ok(n) => rows.truncate(n),
            Err(_) => {
                eprintln!("bench situated: --limit must be an integer");
                return 2;
            }
        }
    }
    if rows.is_empty() {
        eprintln!(
            "bench situated: 0 probes loaded from {} — nothing to judge",
            tpath.display()
        );
        return 4;
    }

    let opts = runner::RunOptions {
        daemon_base,
        judge_model,
        judge_trials,
        // The chaos transcript does not carry the model id, so it is named
        // explicitly and echoed into the report header. Unknown is stated,
        // never guessed.
        subject_model: get_flag(&flags, "subject-model")
            .unwrap_or_else(|| "unspecified".to_string()),
        transcripts_path: tpath.display().to_string(),
        criteria_path: criteria_path.display().to_string(),
        transcripts_skipped: skipped,
    };

    let run = match runner::run(&rows, &vocab, &opts).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bench situated: run failed: {e}");
            return 1;
        }
    };

    if let Some(path) = get_flag(&flags, "report").map(PathBuf::from) {
        if let Err(e) = report::write_json_report(&path, &run) {
            eprintln!("bench situated: failed to write report to {}: {e}", path.display());
            return 1;
        }
        eprintln!("bench situated: report written to {}", path.display());
    }

    if !has_flag(&flags, "json") {
        report::print_text_report(&run);
    }

    if let Some(diff_path) = get_flag(&flags, "diff").map(PathBuf::from) {
        match report::load_report(&diff_path) {
            Ok(baseline) => report::print_diff(&baseline, &run),
            Err(e) => {
                eprintln!("bench situated: failed to load baseline {}: {e}", diff_path.display());
                return 1;
            }
        }
    }

    // A run where nothing got judged proves nothing.
    let judged = run.aggregate.criteria_total - run.aggregate.could_not_judge;
    if judged == 0 {
        eprintln!("bench situated: zero criteria judged — not a result");
        return 4;
    }
    if run.aggregate.degraded {
        return 1;
    }
    0
}

async fn run_calibrate(
    flags: &[(String, String)],
    criteria_path: &std::path::Path,
    vocab: &criteria::Vocabulary,
    daemon_base: &str,
    judge_model: &str,
    judge_trials: u8,
) -> i32 {
    // Calibration lives next to the vocabulary it certifies. This is
    // load-bearing: a judge certified on the moral bank is NOT certified
    // here, and pointing this at the wrong file is how that mistake would
    // happen silently.
    let cal_path = match get_flag(flags, "calibration-file") {
        Some(p) => PathBuf::from(p),
        None => {
            let name = if vocab.meta.calibration_file.is_empty() {
                "calibration.toml"
            } else {
                &vocab.meta.calibration_file
            };
            criteria_path.parent().map(|p| p.join(name)).unwrap_or_default()
        }
    };
    let bank = match judge::load_calibration(&cal_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("bench situated: calibration bank {}: {e}", cal_path.display());
            eprintln!(
                "Hint: the situatedness calibration set is a SEPARATE hand-labeled bank — \
                 the moral lane's set certifies nothing here (criterion families do not \
                 transfer). See bench/situated/CRITERIA_DRAFT.md."
            );
            return 2;
        }
    };
    eprintln!(
        "bench situated: calibrating judge `{judge_model}` on {} labeled items (trials {}) \
         against criteria v{}",
        bank.items.len(),
        judge_trials.max(1),
        vocab.meta.version
    );
    let v1 = format!("{}/v1", daemon_base.trim_end_matches('/'));
    let provider = sovereign_inference::remote::RemoteApiProvider::new(&v1, None, judge_model, 16384);
    let rep = judge::run_calibration(&provider, &bank, Some(judge_model), judge_trials).await;
    judge::print_calibration(&rep, judge_model)
}

fn print_help() {
    eprintln!("svrn bench situated — situatedness process lane (grades HOW a turn situated itself)");
    eprintln!();
    eprintln!("Scores transcripts the chaos bench produced through the PRODUCTION turn.");
    eprintln!("It never generates — there is no bench-local chat loop by design.");
    eprintln!();
    eprintln!("USAGE");
    eprintln!("  svrn bench situated --transcripts <chaos.transcripts.jsonl>");
    eprintln!("                      [--judge-model <id>] [--judge-trials N] [--limit N]");
    eprintln!("                      [--subject-model <id>] [--criteria <path>]");
    eprintln!("                      [--report <path>] [--diff <baseline.json>] [--json]");
    eprintln!("  svrn bench situated --calibrate [--judge-model <id>] [--calibration-file <path>]");
    eprintln!();
    eprintln!("FLAGS");
    eprintln!("  --transcripts <path>     Chaos run transcripts to score (required for scoring).");
    eprintln!("  --subject-model <id>     Model that produced the transcripts, for the header.");
    eprintln!("  --criteria <path>        Override bench/situated/criteria.toml.");
    eprintln!("  --judge-model <id>       Judge model (default: `primary`). PIN THIS and keep it");
    eprintln!("                           identical across runs you intend to compare.");
    eprintln!("  --judge-trials N         Majority vote over N judge calls per criterion (default 1).");
    eprintln!("  --limit N                Score only the first N probes (id order).");
    eprintln!("  --report <path>          Write the full JSON report (per-criterion verdicts).");
    eprintln!("  --diff <baseline.json>   Per-dimension deltas vs a stored report. REFUSES to");
    eprintln!("                           compare across criterion-vocabulary versions.");
    eprintln!("  --json                   Suppress the text report.");
    eprintln!("  --daemon <url>           Daemon base URL (default http://localhost:9741).");
    eprintln!("  --calibrate              Score the judge against the hand-labeled situatedness");
    eprintln!("                           bank; gates on sensitivity/specificity >= 0.85.");
    eprintln!();
    eprintln!("SCORING");
    eprintln!("  Per criterion: yes/no + evidence quote. Per probe: 100 * achieved / max over");
    eprintln!("  signed weights. Criteria are chosen by QUESTION TYPE, never by probe content —");
    eprintln!("  see bench/situated/CRITERIA_DRAFT.md. Failed judge calls are could-not-judge:");
    eprintln!("  counted, reported, never defaulted; >10% degrades the run (exit 1).");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_mode_requires_transcripts() {
        // The lane must refuse rather than invent a generation path — the
        // production-path mandate as a usage error.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let code = rt.block_on(cmd_situated(&["--judge-model".into(), "x".into()]));
        assert_eq!(code, 2, "missing --transcripts must be a usage error, not a run");
    }

    #[test]
    fn help_exits_clean() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert_eq!(rt.block_on(cmd_situated(&["--help".into()])), 0);
    }
}
