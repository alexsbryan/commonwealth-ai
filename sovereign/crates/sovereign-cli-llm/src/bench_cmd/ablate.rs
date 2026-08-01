//! `svrn bench enrichment-ablate` — retrieval-utility A/B lane (T1 P0.4).
//!
//! Answers "which enrichment knobs actually improve answers?" by running the
//! SAME bank through the production retrieval pipeline (`eval run
//! --prod-pipeline --isolate`) under a declared knob matrix, one knob toggled
//! at a time against a baseline arm:
//!
//! | arm             | change vs baseline                                  |
//! |-----------------|-----------------------------------------------------|
//! | baseline        | shipped defaults (env force-cleared for isolation)  |
//! | raptor_off      | `SOVEREIGN_RAPTOR_GROUNDING=0`                      |
//! | conv_ppr_off    | `SOVEREIGN_CONV_PPR_WEIGHT=0`                       |
//! | with_atlas      | `--with-atlas <ids>` (only when `--atlas` given)    |
//!
//! Every rep is a SUBPROCESS so the knob env vars are read fresh by the
//! in-process production pipeline (several are cached at construction time —
//! in-process toggling would silently measure a half-applied config).
//!
//! The deliverable includes the honest negative: a knob whose |Δ mean fact
//! ratio| does not clear both the rep spread and the SP2 band (0.02) is
//! reported as NOT SEPARABLE by that bank — that finding routes to T2's
//! golden-authoring decision (P3.1), it is not padded into a win.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Minimum |Δ| that can count as separation even when rep spread is
/// tiny — the SP2 parity band on the summarize banks (1 fact / 8 q ×
/// 5 facts ≈ 0.025, rounded to a floor both banks share).
const SEPARATION_FLOOR: f64 = 0.02;

/// The knob env vars the matrix owns. Force-cleared from every arm's
/// environment (then selectively set) so an operator's ambient shell
/// exports cannot contaminate the baseline.
const MATRIX_ENV: &[&str] = &[
    "SOVEREIGN_RAPTOR_GROUNDING",
    "SOVEREIGN_CONV_PPR_WEIGHT",
];

const HELP: Help = Help {
    command: "svrn bench enrichment-ablate",
    summary: "Retrieval-utility knob ablation over the production pipeline (T1 P0.4).",
    sections: &[
        HelpSection::Usage(
            "svrn bench enrichment-ablate <bank.toml> [<bank2.toml> ...] \
             [--reps N] [--limit N] [--atlas <ids>] [--runs-dir <dir>] [--output <json>]",
        ),
        HelpSection::Notes(
            "Runs each bank through `eval run --prod-pipeline --isolate` under the \
             declared knob matrix (baseline / raptor_off / conv_ppr_off / \
             doc_cluster_on / with_atlas when --atlas is given), --reps subprocess \
             reps per arm (default 3). --limit is eval run's retrieval pool size \
             per question (default 30 — the SP2 bench register; NOT a question \
             cap, and starving it changes scores). Prints one joined table — \
             mean fact ratio per (bank, arm), Δ vs baseline, and a SEPARABLE / not \
             separable verdict per knob — and writes the full JSON artifact. A knob \
             the banks cannot separate is reported as exactly that (the honest \
             negative feeds T2's golden-authoring decision). Machine-heavy: budget \
             ~1 min per rep per bank.",
        ),
    ],
};

pub async fn cmd_ablate(rest: &[String]) -> i32 {
    if help::wants_help(rest) {
        help::print(&HELP);
        return 0;
    }
    run(rest).await
}

struct ArmSpec {
    name: &'static str,
    env: Vec<(&'static str, String)>,
    extra_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RepResult {
    fact_ratio: f64,
    source_ratio: f64,
    questions: usize,
    output_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct ArmSummary {
    reps: Vec<RepResult>,
    mean_fact: f64,
    min_fact: f64,
    max_fact: f64,
    mean_source: f64,
}

#[derive(Debug, Serialize)]
struct KnobVerdict {
    arm: String,
    delta_vs_baseline: f64,
    baseline_spread: f64,
    arm_spread: f64,
    separable: bool,
}

#[derive(Debug, Serialize)]
struct Artifact {
    generated_by: String,
    reps: usize,
    limit: usize,
    atlas: Option<String>,
    /// bank stem → arm name → summary
    results: BTreeMap<String, BTreeMap<String, ArmSummary>>,
    /// bank stem → verdict per non-baseline arm
    verdicts: BTreeMap<String, Vec<KnobVerdict>>,
}

fn summarize(reps: &[RepResult]) -> ArmSummary {
    let n = reps.len().max(1) as f64;
    let mean_fact = reps.iter().map(|r| r.fact_ratio).sum::<f64>() / n;
    let mean_source = reps.iter().map(|r| r.source_ratio).sum::<f64>() / n;
    let min_fact = reps.iter().map(|r| r.fact_ratio).fold(f64::MAX, f64::min);
    let max_fact = reps.iter().map(|r| r.fact_ratio).fold(f64::MIN, f64::max);
    ArmSummary {
        reps: reps.to_vec(),
        mean_fact,
        min_fact: if reps.is_empty() { 0.0 } else { min_fact },
        max_fact: if reps.is_empty() { 0.0 } else { max_fact },
        mean_source,
    }
}

fn parse_eval_output(path: &std::path::Path) -> Result<RepResult, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| format!("{}: no results array", path.display()))?;
    if results.is_empty() {
        return Err(format!("{}: zero questions scored", path.display()));
    }
    let ratio_of = |q: &serde_json::Value, key: &str| -> f64 {
        q.get(key)
            .and_then(|s| s.get("ratio"))
            .and_then(|r| r.as_f64())
            .unwrap_or(0.0)
    };
    let n = results.len() as f64;
    Ok(RepResult {
        fact_ratio: results.iter().map(|q| ratio_of(q, "fact_score")).sum::<f64>() / n,
        source_ratio: results.iter().map(|q| ratio_of(q, "source_score")).sum::<f64>() / n,
        questions: results.len(),
        output_path: path.display().to_string(),
    })
}

async fn run(rest: &[String]) -> i32 {
    let mut banks: Vec<PathBuf> = Vec::new();
    let mut reps: usize = 3;
    let mut limit: usize = 30;
    let mut atlas: Option<String> = None;
    let mut runs_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match rest.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--reps" => match val!("--reps").parse() {
                Ok(v) if v > 0 => reps = v,
                _ => {
                    eprintln!("error: --reps must be a positive integer");
                    return 2;
                }
            },
            "--limit" => match val!("--limit").parse() {
                Ok(v) if v > 0 => limit = v,
                _ => {
                    eprintln!("error: --limit must be a positive integer");
                    return 2;
                }
            },
            "--atlas" => atlas = Some(val!("--atlas")),
            "--runs-dir" => runs_dir = Some(PathBuf::from(val!("--runs-dir"))),
            "--output" => output = Some(PathBuf::from(val!("--output"))),
            "--help" | "-h" => {
                help::print(&HELP);
                return 0;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
            bank => banks.push(PathBuf::from(bank)),
        }
        i += 1;
    }
    if banks.is_empty() {
        eprintln!("error: at least one <bank.toml> is required");
        help::print(&HELP);
        return 2;
    }
    for b in &banks {
        if !b.exists() {
            eprintln!("error: bank not found: {}", b.display());
            return 2;
        }
    }

    let mut arms: Vec<ArmSpec> = vec![
        ArmSpec {
            name: "baseline",
            env: vec![],
            extra_args: vec![],
        },
        ArmSpec {
            name: "raptor_off",
            env: vec![("SOVEREIGN_RAPTOR_GROUNDING", "0".into())],
            extra_args: vec![],
        },
        ArmSpec {
            name: "conv_ppr_off",
            env: vec![("SOVEREIGN_CONV_PPR_WEIGHT", "0".into())],
            extra_args: vec![],
        },
    ];
    if let Some(ids) = &atlas {
        arms.push(ArmSpec {
            name: "with_atlas",
            env: vec![],
            extra_args: vec!["--with-atlas".into(), ids.clone()],
        });
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: current_exe: {e}");
            return 1;
        }
    };
    let runs_dir = runs_dir.unwrap_or_else(|| PathBuf::from("target/ci-bench/enrichment-ablate"));
    if let Err(e) = std::fs::create_dir_all(&runs_dir) {
        eprintln!("error: create {}: {e}", runs_dir.display());
        return 1;
    }

    let total = banks.len() * arms.len() * reps;
    eprintln!(
        "enrichment-ablate: {} bank(s) × {} arm(s) × {reps} rep(s) = {total} eval runs \
         (~1 min each — this is the machine-heavy lane)",
        banks.len(),
        arms.len(),
    );

    let mut results: BTreeMap<String, BTreeMap<String, ArmSummary>> = BTreeMap::new();
    let mut failures = 0usize;
    let mut done = 0usize;
    for bank in &banks {
        let stem = bank
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| bank.display().to_string());
        for arm in &arms {
            let mut rep_results: Vec<RepResult> = Vec::new();
            for r in 1..=reps {
                let out_json = runs_dir.join(format!("{stem}-{}-r{r}.json", arm.name));
                let log_path = runs_dir.join(format!("{stem}-{}-r{r}.log", arm.name));
                let mut cmd = tokio::process::Command::new(&exe);
                cmd.arg("eval")
                    .arg("run")
                    .arg("--bank")
                    .arg(bank)
                    .arg("--prod-pipeline")
                    .arg("--isolate")
                    .arg("--limit")
                    .arg(limit.to_string())
                    .arg("--format")
                    .arg("json")
                    .arg("--output")
                    .arg(&out_json);
                for a in &arm.extra_args {
                    cmd.arg(a);
                }
                for k in MATRIX_ENV {
                    cmd.env_remove(k);
                }
                for (k, v) in &arm.env {
                    cmd.env(k, v);
                }
                done += 1;
                eprintln!("  [{done}/{total}] {stem} {} r{r} …", arm.name);
                match cmd.output().await {
                    Ok(out) => {
                        let _ = std::fs::write(
                            &log_path,
                            [&out.stdout[..], &out.stderr[..]].concat(),
                        );
                        if !out.status.success() {
                            eprintln!(
                                "    FAIL (exit {:?}) — log: {}",
                                out.status.code(),
                                log_path.display()
                            );
                            failures += 1;
                            continue;
                        }
                    }
                    Err(e) => {
                        eprintln!("    FAIL spawn: {e}");
                        failures += 1;
                        continue;
                    }
                }
                match parse_eval_output(&out_json) {
                    Ok(rep) => rep_results.push(rep),
                    Err(e) => {
                        eprintln!("    FAIL parse: {e}");
                        failures += 1;
                    }
                }
            }
            if rep_results.is_empty() {
                eprintln!(
                    "error: arm `{}` on bank `{stem}` produced zero successful reps — \
                     the joined table would silently misreport this knob; aborting",
                    arm.name
                );
                return 1;
            }
            results
                .entry(stem.clone())
                .or_default()
                .insert(arm.name.to_string(), summarize(&rep_results));
        }
    }

    // Verdicts: each non-baseline arm vs baseline, per bank.
    let mut verdicts: BTreeMap<String, Vec<KnobVerdict>> = BTreeMap::new();
    for (stem, by_arm) in &results {
        let Some(base) = by_arm.get("baseline") else {
            continue;
        };
        let base_spread = base.max_fact - base.min_fact;
        let mut rows = Vec::new();
        for (arm_name, s) in by_arm {
            if arm_name == "baseline" {
                continue;
            }
            let delta = s.mean_fact - base.mean_fact;
            let arm_spread = s.max_fact - s.min_fact;
            let separable = delta.abs() > SEPARATION_FLOOR.max(base_spread).max(arm_spread);
            rows.push(KnobVerdict {
                arm: arm_name.clone(),
                delta_vs_baseline: delta,
                baseline_spread: base_spread,
                arm_spread,
                separable,
            });
        }
        verdicts.insert(stem.clone(), rows);
    }

    println!();
    println!("  Enrichment knob ablation — mean fact ratio (production pipeline)");
    println!("  ─────────────────────────────────────────────────────────────────");
    println!("  bank                    arm             fact    Δ base   verdict");
    for (stem, by_arm) in &results {
        let base_fact = by_arm.get("baseline").map(|b| b.mean_fact).unwrap_or(0.0);
        for (arm_name, s) in by_arm {
            let (delta_str, verdict_str) = if arm_name == "baseline" {
                ("     —".to_string(), String::new())
            } else {
                let v = verdicts
                    .get(stem)
                    .and_then(|rows| rows.iter().find(|r| &r.arm == arm_name));
                (
                    format!("{:+.4}", s.mean_fact - base_fact),
                    match v {
                        Some(r) if r.separable => "SEPARABLE".to_string(),
                        Some(_) => "not separable".to_string(),
                        None => String::new(),
                    },
                )
            };
            println!(
                "  {stem:<22}  {arm_name:<14}  {:.4}  {delta_str}   {verdict_str}",
                s.mean_fact
            );
        }
    }
    if failures > 0 {
        println!("  ({failures} failed rep(s) excluded — see logs in {})", runs_dir.display());
    }

    let artifact = Artifact {
        generated_by: "svrn bench enrichment-ablate".to_string(),
        reps,
        limit,
        atlas,
        results,
        verdicts,
    };
    let out_path =
        output.unwrap_or_else(|| PathBuf::from("target/ci-bench/enrichment-ablate.json"));
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&out_path, serde_json::to_string_pretty(&artifact).unwrap()) {
        Ok(_) => println!("\n  ✓ wrote {}", out_path.display()),
        Err(e) => {
            eprintln!("error: writing {}: {e}", out_path.display());
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(fact: f64) -> RepResult {
        RepResult {
            fact_ratio: fact,
            source_ratio: 1.0,
            questions: 8,
            output_path: String::new(),
        }
    }

    #[test]
    fn summarize_reports_mean_and_spread() {
        let s = summarize(&[rep(0.60), rep(0.62), rep(0.64)]);
        assert!((s.mean_fact - 0.62).abs() < 1e-9);
        assert!((s.min_fact - 0.60).abs() < 1e-9);
        assert!((s.max_fact - 0.64).abs() < 1e-9);
    }

    #[test]
    fn separation_requires_clearing_floor_and_spread() {
        // Δ = 0.015 < floor 0.02 → not separable even with zero spread.
        let delta: f64 = 0.015;
        assert!(delta.abs() <= SEPARATION_FLOOR.max(0.0).max(0.0));
        // Δ = 0.05 with spreads 0.01 → separable.
        let delta2: f64 = 0.05;
        assert!(delta2.abs() > SEPARATION_FLOOR.max(0.01).max(0.01));
        // Δ = 0.05 but baseline spread 0.06 (noisier than the effect) → not separable.
        assert!(delta2.abs() <= SEPARATION_FLOOR.max(0.06).max(0.01));
    }
}
