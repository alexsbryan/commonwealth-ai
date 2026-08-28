// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench uap run|diagnose` — disposition-classification bench
//! for the `uap-blue-book` corpus. The classification analog of
//! `svrn bench enron` (entity resolution).
//!
//! `run` classifies each case's disposition via the daemon chat model
//! (schema-constrained to the era-possible category set), scores against
//! the frozen `gold_labels.jsonl` via
//! `sovereign_eval::disposition_score`, and persists the outcome.
//!
//! Two policies:
//!   - `baseline` — raw case narrative only (the floor).
//!   - `tuned` — narrative + the case's extracted observation features
//!     (shape / location). Both apply the date-conditioned era mask to
//!     the grammar enum, since predicting "Starlink" for a 1952 case is
//!     never legitimate — the mask is a correctness floor, not the tuning
//!     lever. The tuning lever is the feature context.
//!
//! Split discipline (reused verbatim from the Enron bench): the runner
//! refuses to score `holdout` without `--unseal-holdout`, which burns a
//! counter in `peek_budget.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sovereign_eval::disposition_bench::{
    load_fixture_cases, FixtureCase, GoldLabels, PeekBudget, Split,
};
use sovereign_eval::disposition_score::{score_with_axis, DispositionReport, Labeling};
use sovereign_eval::disposition_taxonomy::{era_mask, era_mask_union, year_of};

use corpus_engine::enrichment::pipeline::types::ChatPrompt;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench uap",
    summary: "Disposition-classification bench over the uap-blue-book corpus.",
    sections: &[
        HelpSection::Usage(
            "svrn bench uap run --corpus uap-blue-book --split {train|test|holdout} [--policy {baseline|tuned}] [--base-url <url>] [--no-strip-disposition-tail] [--unseal-holdout] [--bench-dir <path>] [--out <path>]",
        ),
        HelpSection::Subcommands(&[
            ("run", "Classify each case's disposition and score against the frozen gold labels."),
            ("diagnose", "Glass-box: confusion matrix + per-category P/R/F1 + worst over-confused pairs (tuned policy)."),
        ]),
        HelpSection::Notes(
            "Reads case narratives from <bench-dir>/fixtures/cases.jsonl and gold \
             from <bench-dir>/gold_labels.jsonl. Classifies via the daemon at \
             --base-url (defaults to SOVEREIGN_DAEMON_URL, else the configured \
             client_port), constraining the label \
             set to the era-possible categories per case date. baseline = raw \
             narrative; tuned = narrative + extracted features. The synthetic \
             fixture states the disposition in prose; --no-strip-disposition-tail \
             keeps that sentence (default strips it so the baseline isn't trivial).",
        ),
    ],
};

const BENCH_ID: &str = "uap-disposition";
/// The daemon this CLI talks to, resolved through the ONE decider — env
/// (`SOVEREIGN_DAEMON_URL`), then `[daemon] client_port`, then the compiled
/// default. Was a compiled literal, so the flag below was the only way to
/// move it and a session pointed at a second daemon silently missed this
/// verb (§10.6).
fn default_base_url() -> String {
    sovereign_core::setup_config::client_daemon_base()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Baseline,
    Tuned,
}

impl Policy {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "baseline" | "floor" => Some(Policy::Baseline),
            "tuned" => Some(Policy::Tuned),
            _ => None,
        }
    }
    fn as_str(&self) -> &'static str {
        match self {
            Policy::Baseline => "baseline",
            Policy::Tuned => "tuned",
        }
    }
}

#[derive(Debug)]
struct Args {
    corpus: String,
    split: Split,
    policy: Policy,
    unseal_holdout: bool,
    bench_dir: PathBuf,
    out: Option<PathBuf>,
    base_url: String,
    strip_disposition_tail: bool,
    max_tokens: u32,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut corpus: Option<String> = None;
    let mut split: Option<Split> = None;
    let mut policy = Policy::Baseline;
    let mut unseal_holdout = false;
    let mut bench_dir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut base_url: Option<String> = None;
    let mut strip_disposition_tail = true;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => {
                i += 1;
                corpus = Some(
                    args.get(i)
                        .ok_or_else(|| "--corpus requires a value".to_string())?
                        .clone(),
                );
            }
            "--split" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--split requires a value".to_string())?;
                split = Some(match v.as_str() {
                    "train" => Split::Train,
                    "test" => Split::Test,
                    "holdout" => Split::Holdout,
                    other => return Err(format!("unknown split: {other}")),
                });
            }
            "--policy" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--policy requires a value".to_string())?;
                policy = Policy::parse(v)
                    .ok_or_else(|| format!("unknown policy: {v}; expected baseline|tuned"))?;
            }
            "--unseal-holdout" => unseal_holdout = true,
            "--no-strip-disposition-tail" => strip_disposition_tail = false,
            "--bench-dir" => {
                i += 1;
                bench_dir = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--bench-dir requires a value".to_string())?,
                ));
            }
            "--out" => {
                i += 1;
                out = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--out requires a value".to_string())?,
                ));
            }
            "--base-url" => {
                i += 1;
                base_url = Some(
                    args.get(i)
                        .ok_or_else(|| "--base-url requires a value".to_string())?
                        .clone(),
                );
            }
            "--help" | "-h" => {
                help::print(&HELP);
                return Err("__HELP__".into());
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    let corpus = corpus.unwrap_or_else(|| "uap-blue-book".to_string());
    let split = split.ok_or_else(|| "--split is required".to_string())?;
    Ok(Args {
        corpus,
        split,
        policy,
        unseal_holdout,
        bench_dir: bench_dir.unwrap_or_else(default_bench_dir),
        out,
        base_url: base_url.unwrap_or_else(default_base_url),
        strip_disposition_tail,
        max_tokens: 64,
    })
}

fn default_bench_dir() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("sovereign/bench/uap");
        if candidate.exists() {
            return candidate;
        }
        if let Some(parent) = cwd.parent() {
            let candidate = parent.join("sovereign/bench/uap");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("sovereign/bench/uap")
}

use sovereign_core::time::unix_now as now_secs;

fn git_head_short() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── outcome record ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UapBenchOutcome {
    schema_version: u32,
    bench_id: String,
    policy_kind: String,
    split: String,
    corpus: String,
    model_id: String,
    captured_ts_unix: i64,
    accuracy: f64,
    macro_f1: f64,
    n_aligned: usize,
    era_axis: Vec<String>,
    confusion_matrix: serde_json::Value,
    per_category: serde_json::Value,
    source: String,
    delta_from_baseline_accuracy: Option<f64>,
    notes: String,
}

// ── entry ────────────────────────────────────────────────────────────

pub async fn cmd_uap(args: &[String]) -> i32 {
    let Some(first) = args.first() else {
        help::print(&HELP);
        return 2;
    };
    match first.as_str() {
        "run" => match cmd_run(&args[1..]).await {
            Ok(code) => code,
            Err(e) if e == "__HELP__" => 0,
            Err(e) => {
                eprintln!("error: {e}");
                2
            }
        },
        "diagnose" => match cmd_diagnose(&args[1..]).await {
            Ok(code) => code,
            Err(e) if e == "__HELP__" => 0,
            Err(e) => {
                eprintln!("error: {e}");
                2
            }
        },
        "--help" | "-h" => {
            help::print(&HELP);
            0
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            help::print(&HELP);
            2
        }
    }
}

/// Shared classification pass: classify every gold case in the split,
/// returning (predicted labeling, model_id, gold labeling, era axis).
async fn classify_split(
    parsed: &Args,
) -> Result<(Labeling, String, Labeling, Vec<String>), String> {
    // Gold.
    let gold_path = parsed.bench_dir.join("gold_labels.jsonl");
    let gold = GoldLabels::load(&gold_path)
        .map_err(|e| format!("loading {}: {e}", gold_path.display()))?;
    let gold_labeling = gold.as_gold_labeling(parsed.split);
    if gold_labeling.is_empty() {
        return Err(format!(
            "no gold cases in split '{}' (or all sealed) at {}",
            parsed.split.as_str(),
            gold_path.display()
        ));
    }

    // Fixture narratives, keyed by case_id.
    let cases_path = parsed.bench_dir.join("fixtures/cases.jsonl");
    let cases = load_fixture_cases(&cases_path)
        .map_err(|e| format!("loading {}: {e}", cases_path.display()))?;
    let by_id: BTreeMap<String, FixtureCase> =
        cases.into_iter().map(|c| (c.case_id.clone(), c)).collect();

    // Era axis for the split (union of case years).
    let years: Vec<i32> = gold_labeling
        .keys()
        .filter_map(|id| by_id.get(id))
        .filter_map(|c| year_of(&c.date))
        .collect();
    let axis = era_mask_union(years);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let mut predicted: Labeling = Labeling::new();
    let mut model_id = String::new();
    let mut case_ids: Vec<&String> = gold_labeling.keys().collect();
    case_ids.sort();
    for case_id in case_ids {
        let Some(case) = by_id.get(case_id) else {
            // No narrative — can't classify; mark insufficient.
            predicted.insert(case_id.clone(), "INSUFFICIENT_DATA".to_string());
            continue;
        };
        let year = year_of(&case.date).unwrap_or(2024);
        let allowed = era_mask(year);
        let prompt = build_prompt(case, &allowed, parsed.policy, parsed.strip_disposition_tail);
        match run_chat(&client, &parsed.base_url, &prompt, parsed.max_tokens).await {
            Ok(resp) => {
                if model_id.is_empty() {
                    model_id = resp.model.clone();
                }
                let cat = parse_category(&resp.content).unwrap_or_else(|| {
                    eprintln!(
                        "  warn: {case_id}: could not parse category from response: {}",
                        resp.content.chars().take(120).collect::<String>()
                    );
                    "INSUFFICIENT_DATA".to_string()
                });
                predicted.insert(case_id.clone(), cat);
            }
            Err(e) => {
                eprintln!("  warn: {case_id}: classify failed: {e}");
                predicted.insert(case_id.clone(), "INSUFFICIENT_DATA".to_string());
            }
        }
    }

    Ok((predicted, model_id, gold_labeling, axis))
}

fn build_prompt(
    case: &FixtureCase,
    allowed: &[String],
    policy: Policy,
    strip_tail: bool,
) -> ChatPrompt {
    let narrative = if strip_tail {
        strip_disposition_sentence(&case.narrative)
    } else {
        case.narrative.clone()
    };
    let system = "You are adjudicating a historical UAP case into exactly one \
                  disposition category. Decide the single best category from the \
                  allowed list. Return JSON only."
        .to_string();
    let mut user = String::new();
    if policy == Policy::Tuned {
        // Tuned policy: inject the extracted observation features.
        user.push_str("Extracted features:\n");
        if let Some(shape) = &case.shape {
            user.push_str(&format!("- observed shape: {shape}\n"));
        }
        if let Some(loc) = &case.location {
            user.push_str(&format!("- location: {loc}\n"));
        }
        user.push('\n');
    }
    user.push_str("Case narrative:\n");
    user.push_str(&narrative);
    user.push_str(&format!(
        "\n\nAllowed categories: {}.\nRespond as JSON: {{\"category\": \"<one of the allowed>\"}}.",
        allowed.join(", ")
    ));

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "category": { "type": "string", "enum": allowed }
        },
        "required": ["category"],
        "additionalProperties": false
    });
    ChatPrompt::new(system, user).with_response_schema("uap_disposition", schema)
}

/// Strip a trailing "Disposition: X." sentence so the synthetic fixture
/// doesn't hand the classifier its own answer.
fn strip_disposition_sentence(narrative: &str) -> String {
    // Case-insensitive: find the last "Disposition:" and cut from there.
    let lower = narrative.to_ascii_lowercase();
    if let Some(idx) = lower.rfind("disposition:") {
        narrative[..idx].trim_end().to_string()
    } else {
        narrative.to_string()
    }
}

fn parse_category(content: &str) -> Option<String> {
    // Strip think/code fences defensively, then parse {"category": "..."}.
    let cleaned = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```");
    let cleaned = cleaned.split("```").next().unwrap_or(cleaned).trim();
    let v: serde_json::Value = serde_json::from_str(cleaned).ok()?;
    v.get("category")?.as_str().map(|s| s.to_string())
}

// ── run ──────────────────────────────────────────────────────────────

async fn cmd_run(args: &[String]) -> Result<i32, String> {
    let parsed = parse_args(args)?;

    // Holdout gate (reused discipline).
    if parsed.split.requires_unseal() && !parsed.unseal_holdout {
        return Err(format!(
            "refusing to run the '{}' split without --unseal-holdout (peek-budget gated)",
            parsed.split.as_str()
        ));
    }
    if parsed.split.requires_unseal() && parsed.unseal_holdout {
        let budget_path = parsed
            .bench_dir
            .join("baselines")
            .join(BENCH_ID)
            .join("peek_budget.json");
        let mut budget = PeekBudget::load(&budget_path).map_err(|e| format!("peek budget: {e}"))?;
        let n = budget.burn(
            "--unseal-holdout from `svrn bench uap run`",
            git_head_short(),
        );
        budget
            .save(&budget_path)
            .map_err(|e| format!("peek budget save: {e}"))?;
        eprintln!(
            "⚠ holdout unsealed; peek #{n} recorded in {}",
            budget_path.display()
        );
    }

    let (predicted, model_id, gold, axis) = classify_split(&parsed).await?;
    let report = score_with_axis(&predicted, &gold, &axis);

    print_summary(&parsed, &model_id, &report);

    // Delta vs baseline.
    let baseline_path = parsed
        .bench_dir
        .join("baselines")
        .join(BENCH_ID)
        .join("baseline.json");
    let delta = compute_delta_from_baseline(&baseline_path, report.accuracy);

    // Persist.
    let out_path = parsed.out.clone().unwrap_or_else(|| {
        let name = match parsed.policy {
            Policy::Baseline => "baseline.json",
            Policy::Tuned => "latest.json",
        };
        parsed.bench_dir.join("baselines").join(BENCH_ID).join(name)
    });
    let outcome = UapBenchOutcome {
        schema_version: 1,
        bench_id: BENCH_ID.to_string(),
        policy_kind: parsed.policy.as_str().to_string(),
        split: parsed.split.as_str().to_string(),
        corpus: parsed.corpus.clone(),
        model_id,
        captured_ts_unix: now_secs(),
        accuracy: report.accuracy,
        macro_f1: report.macro_f1,
        n_aligned: report.n_aligned,
        era_axis: report.confusion_matrix.categories.clone(),
        confusion_matrix: serde_json::to_value(&report.confusion_matrix).unwrap_or_default(),
        per_category: serde_json::to_value(&report.per_category).unwrap_or_default(),
        source: "fixture".to_string(),
        delta_from_baseline_accuracy: delta,
        notes: format!(
            "Written by `svrn bench uap run`. Narrative source: fixture cases.jsonl. \
             n_aligned={} — small-N fixture, read per-category F1 with the support column.",
            report.n_aligned
        ),
    };
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&outcome).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&out_path, json).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    println!("\nwrote outcome → {}", out_path.display());

    Ok(0)
}

fn compute_delta_from_baseline(baseline_path: &Path, accuracy: f64) -> Option<f64> {
    let text = std::fs::read_to_string(baseline_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let base = v.get("accuracy")?.as_f64()?;
    Some(accuracy - base)
}

fn print_summary(parsed: &Args, model_id: &str, report: &DispositionReport) {
    println!("svrn bench uap — {}", parsed.policy.as_str());
    println!("  corpus:    {}", parsed.corpus);
    println!("  split:     {}", parsed.split.as_str());
    println!(
        "  model:     {}",
        if model_id.is_empty() {
            "(none)"
        } else {
            model_id
        }
    );
    println!("  cases:     {} aligned", report.n_aligned);
    println!("  accuracy:  {:.3}", report.accuracy);
    println!("  macro-F1:  {:.3}", report.macro_f1);
    if !report.unmatched_gold.is_empty() {
        println!("  unmatched gold:      {}", report.unmatched_gold.len());
    }
    if !report.unmatched_predicted.is_empty() {
        println!(
            "  unmatched predicted: {}",
            report.unmatched_predicted.len()
        );
    }
    println!("\n  per-category (P / R / F1 / support):");
    for pc in &report.per_category {
        if pc.support == 0 {
            continue;
        }
        println!(
            "    {:<18} {:.2} / {:.2} / {:.2}  (n={})",
            pc.category, pc.precision, pc.recall, pc.f1, pc.support
        );
    }
}

// ── diagnose ─────────────────────────────────────────────────────────

async fn cmd_diagnose(args: &[String]) -> Result<i32, String> {
    let mut parsed = parse_args(args)?;
    // Diagnose always runs the tuned policy so its numbers match a
    // tuned `run`.
    parsed.policy = Policy::Tuned;

    let (predicted, model_id, gold, axis) = classify_split(&parsed).await?;
    let report = score_with_axis(&predicted, &gold, &axis);

    println!("svrn bench uap diagnose — tuned");
    let model_disp = if model_id.is_empty() {
        "(none)"
    } else {
        model_id.as_str()
    };
    println!(
        "  corpus: {}  split: {}  model: {}",
        parsed.corpus,
        parsed.split.as_str(),
        model_disp
    );
    println!(
        "  accuracy: {:.3}  macro-F1: {:.3}  ({} cases)\n",
        report.accuracy, report.macro_f1, report.n_aligned
    );

    // Confusion matrix grid (gold rows × predicted cols), restricted to
    // categories that actually appear (support>0 or any prediction).
    let cm = &report.confusion_matrix;
    let active: Vec<usize> = (0..cm.categories.len())
        .filter(|&i| {
            let row: usize = cm.matrix[i].iter().sum();
            let col: usize = cm.matrix.iter().map(|r| r[i]).sum();
            row + col > 0
        })
        .collect();
    println!("  confusion matrix (gold ↓ × predicted →):");
    print!("    {:<18}", "");
    for &j in &active {
        print!("{:>6}", short(&cm.categories[j]));
    }
    println!();
    for &i in &active {
        print!("    {:<18}", cm.categories[i]);
        for &j in &active {
            print!("{:>6}", cm.matrix[i][j]);
        }
        println!();
    }

    // Worst over-confused pairs (off-diagonal cells, descending).
    let mut confusions: Vec<(String, String, usize)> = Vec::new();
    for (i, row) in cm.matrix.iter().enumerate() {
        for (j, &cell) in row.iter().enumerate() {
            if i != j && cell > 0 {
                confusions.push((cm.categories[i].clone(), cm.categories[j].clone(), cell));
            }
        }
    }
    confusions.sort_by(|a, b| b.2.cmp(&a.2));
    if !confusions.is_empty() {
        println!("\n  worst confusions (gold → predicted):");
        for (g, p, n) in confusions.iter().take(8) {
            println!("    {g} → {p}  ({n})");
        }
    }

    println!("\n  per-category (P / R / F1 / support):");
    for pc in &report.per_category {
        if pc.support == 0 {
            continue;
        }
        println!(
            "    {:<18} {:.2} / {:.2} / {:.2}  (n={})",
            pc.category, pc.precision, pc.recall, pc.f1, pc.support
        );
    }
    Ok(0)
}

/// Abbreviate a category token for the matrix header (first 5 chars).
fn short(cat: &str) -> String {
    cat.chars().take(5).collect()
}

// ── daemon chat (copied from the atlas bench's run_chat) ──────────────

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponseEnvelope {
    #[serde(default)]
    model: String,
    choices: Vec<ChatChoice>,
}

struct ChatResponse {
    model: String,
    content: String,
}

async fn run_chat(
    client: &reqwest::Client,
    base_url: &str,
    prompt: &ChatPrompt,
    max_tokens: u32,
) -> Result<ChatResponse, String> {
    let url = format!("{}/v1/chat/completions", base_url);
    let mut body = serde_json::json!({
        "model": "",
        "messages": [
            { "role": "system", "content": prompt.system },
            { "role": "user", "content": prompt.user },
        ],
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "stream": false,
    });
    if let Some(schema) = prompt.response_schema.as_ref() {
        let name = prompt
            .response_schema_name
            .as_deref()
            .unwrap_or("response_schema");
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "response_format".into(),
                serde_json::json!({
                    "type": "json_schema",
                    "json_schema": { "name": name, "schema": schema, "strict": true }
                }),
            );
        }
    }
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("body read: {e}"))?;
    if !status.is_success() {
        return Err(format!("daemon HTTP {status}: {text}"));
    }
    let env: ChatResponseEnvelope = serde_json::from_str(&text)
        .map_err(|e| format!("response not JSON: {e} — body: {text}"))?;
    let choice = env
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "response had zero choices".to_string())?;
    Ok(ChatResponse {
        model: env.model,
        content: choice.message.content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_minimum_args_defaults() {
        let p = parse_args(&a(&["--split", "train"])).unwrap();
        assert_eq!(p.corpus, "uap-blue-book");
        assert_eq!(p.split, Split::Train);
        assert_eq!(p.policy, Policy::Baseline);
        assert!(p.strip_disposition_tail);
        assert_eq!(p.base_url, default_base_url());
    }

    #[test]
    fn unknown_split_rejected() {
        assert!(parse_args(&a(&["--split", "weekday"])).is_err());
    }

    #[test]
    fn policy_aliases_parse() {
        assert_eq!(Policy::parse("baseline"), Some(Policy::Baseline));
        assert_eq!(Policy::parse("tuned"), Some(Policy::Tuned));
        assert_eq!(Policy::parse("nonsense"), None);
    }

    #[test]
    fn holdout_without_unseal_is_rejected_by_gate() {
        // The gate lives in cmd_run; here we just assert the flag parses.
        let p = parse_args(&a(&["--split", "holdout"])).unwrap();
        assert!(p.split.requires_unseal());
        assert!(!p.unseal_holdout);
    }

    #[test]
    fn strip_disposition_sentence_removes_tail() {
        let n = "Witnesses saw a light. Disposition: AIRCRAFT.";
        assert_eq!(strip_disposition_sentence(n), "Witnesses saw a light.");
        let no_tail = "Witnesses saw a light.";
        assert_eq!(strip_disposition_sentence(no_tail), no_tail);
    }

    #[test]
    fn parse_category_handles_fences() {
        assert_eq!(
            parse_category(r#"{"category": "BALLOON"}"#).as_deref(),
            Some("BALLOON")
        );
        assert_eq!(
            parse_category("```json\n{\"category\": \"HOAX\"}\n```").as_deref(),
            Some("HOAX")
        );
        assert_eq!(parse_category("not json").as_deref(), None);
    }
}
