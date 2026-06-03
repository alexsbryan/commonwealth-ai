//! A/B driver comparison: `diff`, `run-ab`, `probe-driver`.
//!
//! - **`atos diff <id>`** reads the per-run `atos_tool_events` for a
//!   given milestone and prints a per-tool side-by-side table so the
//!   operator can see how Claude Code and opencode approached the
//!   same problem.
//! - **`atos run-ab <id> --brief <path>`** is the write-amplifier:
//!   spawns each driver against the same milestone in sequence,
//!   then runs `diff` against the resulting runs.
//! - **`atos probe-driver`** is a standalone tool-use sanity check
//!   that POSTs a trivial function-calling request to
//!   `/v1/chat/completions` and reports whether the server emitted
//!   structured `tool_calls`. Used from `doctor` and from CI.

use sovereign_atos::AtosOrchestrator;

use super::args::{get_flag, split_args};
use super::stores::open_orchestrator;

// ─── diff ────────────────────────────────────────────────────────────────────

pub(crate) async fn cmd_diff(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("diff: missing <feature-id>");
        return 2;
    };
    let ordinal: Option<i64> = get_flag(&flags, "--ordinal").and_then(|s| s.parse::<i64>().ok());

    let orc = match open_orchestrator() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("diff: {e}");
            return 1;
        }
    };

    let Some(feature) = orc.get_feature(&feature_id).await.ok().flatten() else {
        eprintln!("diff: feature '{feature_id}' not found");
        return 1;
    };
    let milestones = orc.list_milestones(&feature_id).await.unwrap_or_default();
    let target_milestone = match ordinal {
        Some(n) => milestones.iter().find(|m| m.ordinal == n).cloned(),
        None => milestones.last().cloned(),
    };
    let Some(milestone) = target_milestone else {
        eprintln!("diff: no milestones for feature '{feature_id}'");
        return 1;
    };

    let runs_all = orc.list_runs(&feature_id).await.unwrap_or_default();
    let runs: Vec<_> = runs_all
        .into_iter()
        .filter(|r| r.milestone_id == milestone.id)
        .collect();
    if runs.is_empty() {
        eprintln!(
            "diff: no runs for feature '{feature_id}' milestone {}",
            milestone.ordinal
        );
        return 1;
    }

    render_diff(&feature, &milestone, &runs, orc.features()).await;
    0
}

/// Count tool events per (tool_name, run_id) with a breakdown of
/// outcomes and parse errors. Keyed on `tool_name` so the diff view
/// can render one row per tool across drivers.
#[derive(Default, Debug)]
struct ToolCounts {
    after: usize,
    errors: usize,
    parse_errors: usize,
    total_duration_ms: i64,
}

async fn render_diff(
    feature: &corpus_engine_atos::FeatureRow,
    milestone: &corpus_engine_atos::MilestoneRow,
    runs: &[corpus_engine_atos::AtosRunRow],
    feature_store: &std::sync::Arc<corpus_engine_atos::FeatureStore>,
) {
    println!();
    println!("  ── atos diff ────────────────────────────────────────────────");
    println!("  Feature:   {}", feature.id);
    println!("  Milestone: {}", milestone.ordinal);
    println!("  Runs:");
    for r in runs {
        let duration = match (r.started_at, r.ended_at) {
            (s, Some(e)) => format!("{}s", e - s),
            _ => "in-flight".into(),
        };
        let verdict = match r.stop_passed {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "?",
        };
        println!(
            "    • {:9} [{}]  driver={:8}  duration={:>7}",
            &r.id[..r.id.len().min(8)],
            verdict,
            r.driver,
            duration
        );
    }

    // Aggregate per-driver counts. Multiple runs for the same driver
    // (e.g. retries) collapse into one column — what the operator
    // wants is "how does claude typically behave here vs opencode."
    use std::collections::BTreeMap;
    let mut per_driver: BTreeMap<String, BTreeMap<String, ToolCounts>> = BTreeMap::new();
    for run in runs {
        let events = feature_store
            .list_events_for_run(&run.id)
            .await
            .unwrap_or_default();
        let entry = per_driver.entry(run.driver.clone()).or_default();
        for e in events {
            let counts = entry.entry(e.tool_name.clone()).or_default();
            match e.phase.as_str() {
                "after" => {
                    counts.after += 1;
                    if matches!(e.outcome.as_deref(), Some("error")) {
                        counts.errors += 1;
                    }
                    if let Some(d) = e.duration_ms {
                        counts.total_duration_ms += d;
                    }
                }
                "parse_error" => {
                    counts.parse_errors += 1;
                }
                _ => {}
            }
        }
    }

    // Union of tool names across drivers, sorted for stable output.
    let mut all_tools: std::collections::BTreeSet<String> = Default::default();
    for m in per_driver.values() {
        for k in m.keys() {
            all_tools.insert(k.clone());
        }
    }

    // Drivers we print columns for, in a stable order (claude first
    // so it's the baseline column on the left).
    let drivers: Vec<&String> = {
        let mut v: Vec<&String> = per_driver.keys().collect();
        v.sort_by(|a, b| {
            // claude < opencode < any others alphabetically
            let rank = |s: &str| match s {
                "claude" => 0,
                "opencode" => 1,
                _ => 2,
            };
            rank(a).cmp(&rank(b)).then(a.cmp(b))
        });
        v
    };

    println!();
    println!("  Per-tool activity:");
    let mut header = String::from("    tool                    ");
    for d in &drivers {
        header.push_str(&format!("{:>10}", d));
    }
    header.push_str("   note");
    println!("{header}");
    println!(
        "    ─────────────────────── {}   ─────────────────────────────",
        "──────────".repeat(drivers.len())
    );

    for tool in &all_tools {
        let mut line = format!("    {:<24}", truncate(tool, 24));
        let mut counts: Vec<usize> = Vec::new();
        let mut parse_err_here = false;
        for d in &drivers {
            let c = per_driver
                .get(*d)
                .and_then(|m| m.get(tool))
                .map(|c| (c.after, c.parse_errors))
                .unwrap_or((0, 0));
            counts.push(c.0);
            if c.1 > 0 {
                parse_err_here = true;
            }
            if c.1 > 0 {
                line.push_str(&format!("{:>8}×e{}", c.0, c.1));
            } else {
                line.push_str(&format!("{:>10}", format!("{}×", c.0)));
            }
        }
        let note = classify_delta(&counts, parse_err_here);
        line.push_str(&format!("   {note}"));
        println!("{line}");
    }

    // Also surface a parse-error total so the operator sees it even
    // if no per-tool row has parse errors (they could land on a
    // tool the other driver didn't touch).
    let total_parse_errors: usize = per_driver
        .values()
        .flat_map(|m| m.values())
        .map(|c| c.parse_errors)
        .sum();
    if total_parse_errors > 0 {
        println!();
        println!(
            "  ⚠ {total_parse_errors} tool_call parse error(s) across all runs — \
             run `sovereign atos diff {} --ordinal {} --verbose` (TODO) to inspect payloads.",
            feature.id, milestone.ordinal
        );
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.into()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

/// Heuristic annotation for the diff table. Kept simple and
/// iteration-friendly — this is the first lever we'll tune once we
/// have real comparison data.
fn classify_delta(counts: &[usize], parse_errors: bool) -> String {
    if parse_errors {
        return "parse failure (see above)".into();
    }
    if counts.len() < 2 {
        return String::new();
    }
    let max = counts.iter().copied().max().unwrap_or(0);
    let min = counts.iter().copied().min().unwrap_or(0);
    if max == 0 {
        return String::new();
    }
    if min == 0 {
        return "only one driver used this tool".into();
    }
    let ratio = max as f64 / min as f64;
    if ratio >= 3.0 && max - min >= 3 {
        "large delta — inspect".into()
    } else if max - min <= 2 {
        "close".into()
    } else {
        "moderate delta".into()
    }
}

// ─── run-ab ──────────────────────────────────────────────────────────────────

pub(crate) async fn cmd_run_ab(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("run-ab: missing <feature-id>");
        return 2;
    };
    let Some(brief_path) = get_flag(&flags, "--brief") else {
        eprintln!("run-ab: --brief <path> is required");
        return 2;
    };
    let drivers_flag = get_flag(&flags, "--drivers").unwrap_or_else(|| "claude,opencode".into());
    let drivers: Vec<String> = drivers_flag
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    if drivers.is_empty() {
        eprintln!("run-ab: --drivers cannot be empty");
        return 2;
    }
    // Read brief once up-front so a path typo fails fast.
    if let Err(e) = std::fs::read_to_string(&brief_path) {
        eprintln!("run-ab: read {brief_path}: {e}");
        return 1;
    }

    println!("run-ab: drivers = {}", drivers.join(", "));
    let mut all_passed = true;
    for (idx, d) in drivers.iter().enumerate() {
        println!();
        println!("── driver={d} ───────────────────────────────────────");
        // Both drivers attach to the same milestone so `atos diff`
        // shows a real side-by-side view. The first driver creates
        // the milestone; subsequent drivers reuse it.
        let mut start_args = vec![
            feature_id.clone(),
            "--brief".into(),
            brief_path.clone(),
            "--driver".into(),
            d.clone(),
        ];
        if idx > 0 {
            start_args.push("--reuse-last-milestone".into());
        }
        let rc = super::milestone::cmd_start_milestone(&start_args).await;
        if rc != 0 {
            eprintln!("run-ab: driver '{d}' exited non-zero ({rc})");
            all_passed = false;
        }
        let end_rc = super::milestone::cmd_end_milestone(std::slice::from_ref(&feature_id)).await;
        if end_rc != 0 {
            all_passed = false;
        }
    }

    println!();
    println!("── diff ──────────────────────────────────────────────");
    cmd_diff(&[feature_id]).await;

    if all_passed {
        0
    } else {
        1
    }
}

// ─── probe-driver ────────────────────────────────────────────────────────────

pub(crate) async fn cmd_probe_driver(args: &[String]) -> i32 {
    let (_positional, flags) = split_args(args);
    let url = get_flag(&flags, "--url")
        .unwrap_or_else(|| "http://localhost:9741/v1/chat/completions".to_string());

    let probe = serde_json::json!({
        "model": "probe",
        "messages": [
            {"role": "user", "content": "Call the ping tool with {} as args."}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "ping",
                "description": "Trivial probe tool.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        }],
        "tool_choice": "required",
        "max_tokens": 64,
        "stream": false
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            eprintln!("probe-driver: reqwest init: {e}");
            return 1;
        }
    };

    println!("probe-driver: POST {url}");
    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(probe.to_string())
        .send()
        .await;
    let body = match res {
        Ok(r) => {
            let status = r.status();
            match r.text().await {
                Ok(b) => {
                    println!("probe-driver: HTTP {}", status.as_u16());
                    if !status.is_success() {
                        println!("{b}");
                        return 1;
                    }
                    b
                }
                Err(e) => {
                    eprintln!("probe-driver: body read: {e}");
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("probe-driver: request failed: {e}");
            return 1;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("probe-driver: parse response: {e}");
            return 1;
        }
    };

    // Accept both the structured `tool_calls` field (what M2
    // introduces) and the fallback text-in-content form (pre-M2
    // servers). The structured form is the success case.
    let tool_calls = parsed
        .pointer("/choices/0/message/tool_calls")
        .and_then(|v| v.as_array());
    match tool_calls {
        Some(calls) if !calls.is_empty() => {
            println!(
                "probe-driver: PASS — server emitted {} structured tool_call(s)",
                calls.len()
            );
            // Print the first call so operators see what the model produced.
            if let Some(first) = calls.first() {
                println!("  {first}");
            }
            0
        }
        _ => {
            println!("probe-driver: FAIL — response did not include structured tool_calls.");
            println!(
                "  Full message: {}",
                parsed
                    .pointer("/choices/0/message")
                    .unwrap_or(&serde_json::Value::Null)
            );
            1
        }
    }
}
