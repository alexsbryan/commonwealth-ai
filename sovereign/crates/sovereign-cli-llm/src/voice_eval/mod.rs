//! `sovereign voice eval` — Tier-B harness for the glass-box voice
//! contract.
//!
//! Drives one or all scenarios under `sovereign/bench/voice/*.toml`
//! through the local Runtime and scores each response on:
//!
//!   1. **Deterministic checks** (regex + counting): length cap,
//!      question density, banned-phrase scan, required-substring
//!      match. Cheap, no inference, fully unit-tested.
//!   2. **LLM-as-judge** (`SampleSelector::voice_judge` with
//!      `JudgePreset::Voice`): scores the response on the eight
//!      principles + four avoid-list patterns from the relational
//!      voice contract. One Fast-slot call per scenario.
//!
//! Outputs JSON + a text report; tracks per-scenario per-axis
//! scores so a refinement run can be diffed against a prior run.
//!
//! Mirrors the structure of `awareness_cmd/eval.rs` (golden-set
//! scoring) and `enrich_cmd/eval.rs` (atlas scoring) — both already
//! ship the precision/recall/F1 → JSON pattern this builds on.
//!
//! ## Subcommand surface
//!
//! ```text
//! sovereign voice eval --scenario <id>            # run one
//! sovereign voice eval --all                      # run all
//! sovereign voice eval --scenario X --canned-response "..."
//!                                                 # dry-run: skip
//!                                                 # the live runtime,
//!                                                 # score the canned
//!                                                 # response only
//! sovereign voice eval --report path/to/out.json  # write JSON report
//! ```
//!
//! The `--canned-response` form is what makes the harness testable
//! end-to-end without a running daemon — it exercises the loader,
//! the deterministic-check pipeline, the report writer, and (when
//! present) the judge. The "live" path additionally drives the
//! Runtime and is wired through `ChatSession::build_session` (same
//! pattern `eval_cmd::runner::run_bank_synth` uses).

pub mod checks;
pub mod judge;
pub mod report;
pub mod runner;
pub mod scenarios;

use std::path::{Path, PathBuf};

/// Boolean flags that do not consume the next token. Mirrors the
/// awareness-cmd args helper, but inlined so `voice_eval` doesn't
/// depend on the dev-tools-gated awareness module.
const BOOLEAN_FLAGS: &[&str] = &["all", "json", "help", "h"];

fn split_args(args: &[String]) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
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
            positional.push(arg.clone());
            i += 1;
        }
    }
    (positional, flags)
}

fn get_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

fn has_flag(flags: &[(String, String)], name: &str) -> bool {
    let key = name.strip_prefix("--").unwrap_or(name);
    flags.iter().any(|(k, _)| k == key)
}

/// Default location for scenario TOMLs, resolved relative to the
/// repo root. Override via `--scenarios-dir`.
const DEFAULT_SCENARIOS_DIR: &str = "bench/voice";

pub async fn run_voice_eval(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);

    // First positional is the action — only `eval` today; reserved
    // surface for future `voice list` / `voice describe`.
    let action = positional.first().map(|s| s.as_str()).unwrap_or("eval");
    if action != "eval" {
        eprintln!("voice: unknown action `{action}`. Try `voice eval --help`.");
        return 2;
    }

    if has_flag(&flags, "help") || has_flag(&flags, "h") {
        print_help();
        return 0;
    }

    // Resolve scenarios directory. Honour `--scenarios-dir`, else
    // walk up from CWD looking for `bench/voice/`. Fail clearly if
    // neither path exists — there's no graceful fallback for "no
    // scenarios available".
    let scenarios_dir = match resolve_scenarios_dir(&flags) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("voice eval: {e}");
            return 2;
        }
    };

    // Selection: --all (every scenario), --scenario <id> (one), or
    // first-positional fallback.
    let scenarios_to_run = match select_scenarios(&scenarios_dir, &flags, &positional[1..]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("voice eval: {e}");
            return 2;
        }
    };

    if scenarios_to_run.is_empty() {
        eprintln!("voice eval: no scenarios matched the selection.");
        return 2;
    }

    // Mode: dry-run (`--canned-response "<text>"`) skips the live
    // Runtime. Useful for harness validation / CI without a daemon.
    let canned_response = get_flag(&flags, "canned-response");
    let report_path: Option<PathBuf> = get_flag(&flags, "report")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let json_only = has_flag(&flags, "json");
    let no_judge = has_flag(&flags, "no-judge");
    let daemon_base = get_flag(&flags, "daemon").filter(|s| !s.is_empty());
    let skills_dir_override = get_flag(&flags, "skills-dir-skills")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    // Model A/B flags. `--chat-model` swaps the runtime turn's model;
    // `--judge-model` pins the judge to a stable rater so chat-model
    // variance doesn't get conflated with judge variance. Either may
    // be omitted (then the daemon's configured chat model is used,
    // and the judge runs on whatever the chat call uses).
    let chat_model = get_flag(&flags, "chat-model").filter(|s| !s.is_empty());
    let judge_model = get_flag(&flags, "judge-model").filter(|s| !s.is_empty());

    let mut run = report::VoiceEvalRun::new().with_models(chat_model.clone(), judge_model.clone());

    if let Some(text) = canned_response {
        // Dry-run path — score the canned response against every
        // scenario's deterministic checks. No inference, no daemon.
        for scenario in &scenarios_to_run {
            let result = checks::run_checks(scenario, &text);
            run.add(result);
        }
    } else {
        // Live path — drive each scenario through a daemon-backed
        // Runtime, capture the streamed response, score it.
        let opts = runner::LiveRunOptions {
            daemon_base,
            judge: !no_judge,
            skills_dir: skills_dir_override,
            chat_model,
            judge_model,
        };
        match runner::run_live(&scenarios_to_run, &opts).await {
            Ok(live_results) => {
                for live in live_results {
                    // Judge scores: emit them inline so an operator
                    // sees them mid-run, then fold them into the run
                    // via `add_live` so the JSON report carries the
                    // per-scenario axes + latency.
                    if let Some(score) = &live.judge {
                        eprintln!(
                            "  judge: attn={} spec={} cal={} q={} sil={} dis={} edge={} hon={} avoid={}",
                            score.right_attention,
                            score.right_specificity,
                            score.right_calibration,
                            score.right_question,
                            score.right_silence,
                            score.right_disagreement,
                            score.right_edge,
                            score.right_self_honesty,
                            score.avoid_list_penalty,
                        );
                        if !score.rationale.is_empty() {
                            eprintln!("  rationale: {}", score.rationale);
                        }
                    }
                    run.add_live(
                        live.result,
                        live.judge,
                        live.runtime_ms,
                        live.judge_ms,
                        live.metrics,
                    );
                }
            }
            Err(e) => {
                eprintln!("voice eval: live run failed: {e}");
                eprintln!(
                    "Hint: ensure the sovereign daemon is running (`sovereign daemon start`) \
                     and the bundled skills directory is reachable. Pass --canned-response \
                     to skip the live path."
                );
                return 1;
            }
        }
    }

    // Write report.
    if let Some(path) = report_path.as_ref() {
        if let Err(e) = report::write_json_report(path, &run) {
            eprintln!(
                "voice eval: failed to write report to {}: {e}",
                path.display()
            );
            return 1;
        }
    }

    if !json_only {
        report::print_text_report(&run);
    }

    // Axis-level diff against a baseline. The tuning loop's primary
    // signal: prompt-edit X moved right_silence by Y. Pass/fail
    // flips have run-to-run variance the README documents at ±2-4
    // scenarios; axis means pool across all scenarios so they're
    // more stable.
    if let Some(diff_path) = get_flag(&flags, "diff").filter(|s| !s.is_empty()) {
        let baseline_path = std::path::PathBuf::from(&diff_path);
        match report::load_axis_means_from_report(&baseline_path) {
            Ok(baseline) => match report::AxisMeans::from_run(&run) {
                Some(current) => report::print_axis_diff(&baseline, &current),
                None => {
                    eprintln!(
                        "voice eval: --diff requested but current run has no judge scores — \
                         was --no-judge in effect?"
                    );
                }
            },
            Err(e) => {
                eprintln!(
                    "voice eval: failed to load baseline from {}: {e}",
                    baseline_path.display()
                );
            }
        }
    }

    if run.has_failures() {
        1
    } else {
        0
    }
}

fn print_help() {
    eprintln!("sovereign voice eval — score the relational voice contract");
    eprintln!();
    eprintln!("USAGE");
    eprintln!("  sovereign voice eval [--scenario <id> | --skill <id> | --all]");
    eprintln!("                       [--scenarios-dir <path>] [--canned-response \"...\"]");
    eprintln!("                       [--report <path>] [--chat-model <id>] [--judge-model <id>]");
    eprintln!();
    eprintln!("FLAGS");
    eprintln!("  --scenario <id>            Run only the named scenario.");
    eprintln!("  --skill <id>               Run every scenario whose [scenario].skill matches.");
    eprintln!("                             E.g. `--skill inner-work` filters out the personal-");
    eprintln!("                             assistant scenarios so a baseline reflects one skill.");
    eprintln!("  --all                      Run every scenario in the scenarios dir.");
    eprintln!("  --canned-response \"...\"    Dry-run: skip the live Runtime and score the");
    eprintln!("                             passed text against the scenario's checks. Useful");
    eprintln!("                             for CI and harness validation.");
    eprintln!("  --scenarios-dir <path>     Override the default `bench/voice/` location.");
    eprintln!("  --report <path>            Write the per-scenario JSON report to this path.");
    eprintln!("  --diff <baseline.json>     After the run, print per-axis deltas against the");
    eprintln!("                             baseline JSON. Axis means are the tuning loop's");
    eprintln!("                             primary signal — pass/fail flips have run-to-run");
    eprintln!("                             variance, axis means pool across scenarios.");
    eprintln!("  --json                     Suppress the text report; print only the JSON path.");
    eprintln!(
        "  --no-judge                 Skip the LLM-as-judge call; deterministic checks only."
    );
    eprintln!("  --chat-model <id>          Pin the runtime turn to this model id (gguf stem).");
    eprintln!("  --judge-model <id>         Pin the LLM-as-judge to this model id, regardless of");
    eprintln!("                             --chat-model. Use this for model A/B baselines so the");
    eprintln!("                             judge stays stable while the chat model varies.");
    eprintln!("  --daemon <url>             Daemon base URL (default from SetupConfig).");
    eprintln!();
    eprintln!("SCORING");
    eprintln!("  Deterministic checks (length cap, question density, banned phrases,");
    eprintln!("  required substrings) run on every response. The voice-judge LLM rubric");
    eprintln!("  (eight principles + avoid-list) is exposed via `judge::voice_judge_request`");
    eprintln!("  and used by the live runner — see `executor::VOICE_JUDGE_PROMPT`.");
}

fn resolve_scenarios_dir(flags: &[(String, String)]) -> Result<PathBuf, String> {
    if let Some(explicit) = get_flag(flags, "scenarios-dir").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(explicit);
        return if p.is_dir() {
            Ok(p)
        } else {
            Err(format!(
                "--scenarios-dir `{}` is not a directory",
                p.display()
            ))
        };
    }

    // Walk up from CWD looking for bench/voice/.
    let mut here: PathBuf =
        std::env::current_dir().map_err(|e| format!("cannot resolve current dir: {e}"))?;
    loop {
        let candidate = here.join(DEFAULT_SCENARIOS_DIR);
        if candidate.is_dir() {
            return Ok(candidate);
        }
        if !here.pop() {
            break;
        }
    }
    Err(format!(
        "could not find `{DEFAULT_SCENARIOS_DIR}` walking up from CWD. Pass --scenarios-dir."
    ))
}

fn select_scenarios(
    dir: &Path,
    flags: &[(String, String)],
    positional: &[String],
) -> Result<Vec<scenarios::Scenario>, String> {
    let all = has_flag(flags, "all");
    let by_flag = get_flag(flags, "scenario").filter(|s| !s.is_empty());
    let by_positional = positional.iter().find(|s| !s.starts_with("--")).cloned();
    let target_id = by_flag.or(by_positional);
    // `--skill <id>` filters the loaded set to scenarios whose
    // `[scenario].skill` matches. Composes with `--all` (run every
    // scenario for that skill) and is mutually exclusive with
    // `--scenario` (since a single id already pins the skill).
    let skill_filter = get_flag(flags, "skill").filter(|s| !s.is_empty());

    if all && target_id.is_some() {
        return Err("pass either --all or --scenario <id>, not both".into());
    }
    if skill_filter.is_some() && target_id.is_some() {
        return Err("pass either --skill <id> or --scenario <id>, not both".into());
    }

    let loaded = scenarios::load_all(dir)?;

    if let Some(skill) = skill_filter.as_deref() {
        // --skill is the new first-class filter. Combine with --all
        // (or treat as implicit --all when neither is passed) to run
        // every scenario for that skill.
        let filtered: Vec<scenarios::Scenario> = loaded
            .into_iter()
            .filter(|s| s.scenario.skill == skill)
            .collect();
        if filtered.is_empty() {
            return Err(format!(
                "no scenarios in {} declare skill = \"{skill}\"",
                dir.display()
            ));
        }
        return Ok(filtered);
    }

    if all {
        return Ok(loaded);
    }

    let id = target_id.ok_or_else(|| {
        "no scenario selected. Pass --scenario <id>, --skill <id>, or --all.".to_string()
    })?;

    let one = loaded
        .into_iter()
        .find(|s| s.scenario.id == id)
        .ok_or_else(|| format!("scenario `{id}` not found in {}", dir.display()))?;
    Ok(vec![one])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_scenario(dir: &std::path::Path, id: &str, skill: &str) {
        let body = format!(
            r#"[scenario]
id = "{id}"
skill = "{skill}"
description = "test scenario for {id}"

[turn]
user = "hello"

[expected]
"#
        );
        fs::write(dir.join(format!("{id}.toml")), body).unwrap();
    }

    #[test]
    fn select_by_skill_filters_to_matching_scenarios() {
        let dir = tempfile::tempdir().unwrap();
        write_scenario(dir.path(), "i01", "inner-work");
        write_scenario(dir.path(), "i02", "inner-work");
        write_scenario(dir.path(), "p01", "personal-assistant");

        let flags = vec![("skill".to_string(), "inner-work".to_string())];
        let picked = select_scenarios(dir.path(), &flags, &[]).unwrap();
        let ids: Vec<&str> = picked.iter().map(|s| s.scenario.id.as_str()).collect();
        assert_eq!(ids, vec!["i01", "i02"]);
    }

    #[test]
    fn select_by_skill_with_no_matches_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_scenario(dir.path(), "i01", "inner-work");
        let flags = vec![("skill".to_string(), "research".to_string())];
        let err = select_scenarios(dir.path(), &flags, &[]).unwrap_err();
        assert!(err.contains("no scenarios"));
        assert!(err.contains("research"));
    }

    #[test]
    fn select_by_skill_and_scenario_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write_scenario(dir.path(), "i01", "inner-work");
        let flags = vec![
            ("skill".to_string(), "inner-work".to_string()),
            ("scenario".to_string(), "i01".to_string()),
        ];
        let err = select_scenarios(dir.path(), &flags, &[]).unwrap_err();
        assert!(err.contains("--skill"));
        assert!(err.contains("--scenario"));
    }

    #[test]
    fn select_all_still_returns_every_scenario() {
        let dir = tempfile::tempdir().unwrap();
        write_scenario(dir.path(), "i01", "inner-work");
        write_scenario(dir.path(), "p01", "personal-assistant");
        let flags = vec![("all".to_string(), String::new())];
        let picked = select_scenarios(dir.path(), &flags, &[]).unwrap();
        assert_eq!(picked.len(), 2);
    }
}
