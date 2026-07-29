// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn knowledge-gym …` — correctness harness for the
//! unified `knowledge_lookup` tool (Tool-Mastery framework Phase 5).
//!
//! Each fixture under `sovereign/bench/knowledge-gym/fixtures/`
//! is a self-contained replay: an `input.json` (OpenAI-compatible
//! chat completion request with the `knowledge_lookup` tool
//! declared), a `mock_evidence.json` (the canned response we
//! return when the model emits a `knowledge_lookup` call), and a
//! `pass.toml` (the predicates to evaluate against the transcript).
//!
//! The gym hits the running daemon at `/v1/chat/completions`,
//! observes the model's `tool_calls`, fields the
//! `knowledge_lookup` invocations with the fixture's mock
//! evidence (no live tool execution), and scores the transcript
//! against structural + semantic predicates.

mod runner;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const DEFAULT_BASE_URL: &str = "http://localhost:9741";
const DEFAULT_FIXTURES_DIR: &str = "sovereign/bench/knowledge-gym/fixtures";
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_REPLAYS: u32 = 3;

pub async fn run_knowledge_gym(args: &[String]) -> i32 {
    let cmd = args.first().map(String::as_str).unwrap_or("run");
    match cmd {
        "run" => run_cmd(&args[1..]).await,
        "-h" | "--help" | "help" => {
            print_help();
            0
        }
        other => {
            eprintln!("unknown subcommand: {other}\n");
            print_help();
            2
        }
    }
}

fn print_help() {
    println!(
        "svrn knowledge-gym — correctness harness for knowledge_lookup\n\n\
         USAGE\n  sovereign knowledge-gym run [--fixture SLUG] [--replays N] \
         [--base-url URL] [--fixtures-dir PATH] [--json]\n\n\
         Each fixture dir holds input.json + mock_evidence.json + pass.toml.\n\
         The gym replays the input against the running daemon, mocks the\n\
         knowledge_lookup tool with the canned evidence, and scores the\n\
         transcript."
    );
}

async fn run_cmd(args: &[String]) -> i32 {
    let mut base_url = DEFAULT_BASE_URL.to_string();
    let mut fixtures_dir: PathBuf = DEFAULT_FIXTURES_DIR.into();
    let mut only_fixture: Vec<String> = Vec::new();
    let mut replays: u32 = DEFAULT_REPLAYS;
    let mut json_out = false;

    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--base-url" => {
                base_url = iter.next().cloned().unwrap_or_else(|| {
                    eprintln!("--base-url requires a value");
                    std::process::exit(2)
                });
            }
            "--fixtures-dir" => {
                fixtures_dir = iter.next().map(PathBuf::from).unwrap_or_else(|| {
                    eprintln!("--fixtures-dir requires a value");
                    std::process::exit(2)
                });
            }
            "--fixture" => {
                only_fixture.push(iter.next().cloned().unwrap_or_else(|| {
                    eprintln!("--fixture requires a value");
                    std::process::exit(2)
                }));
            }
            "--replays" => {
                let v = iter.next().cloned().unwrap_or_else(|| {
                    eprintln!("--replays requires a value");
                    std::process::exit(2)
                });
                replays = v.parse().unwrap_or_else(|_| {
                    eprintln!("--replays expects an integer");
                    std::process::exit(2)
                });
            }
            "--json" => {
                json_out = true;
            }
            _ => {
                eprintln!("unknown flag: {a}");
                return 2;
            }
        }
    }

    let fixtures = match load_fixtures(&fixtures_dir, &only_fixture) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("knowledge-gym: cannot load fixtures: {e}");
            return 1;
        }
    };
    if fixtures.is_empty() {
        eprintln!(
            "knowledge-gym: no fixtures found under {}",
            fixtures_dir.display()
        );
        return 1;
    }

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("build http client");
    let cfg = runner::RunnerCfg { base_url, replays };

    // The human report is the primary payload and belongs on stdout so
    // `svrn ... > gym.txt` captures it. Under `--json` stdout is already
    // owned by the machine-readable document, so the same lines fall back
    // to stderr rather than corrupting it.
    let emit = |line: &str| {
        if json_out {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };

    let mut all_runs: Vec<runner::FixtureReport> = Vec::new();
    for fx in &fixtures {
        emit(&format!("\n=== fixture: {} ===", fx.slug));
        let report = runner::run_fixture_replays(&client, &cfg, fx).await;
        for line in report.human_lines() {
            emit(&format!("  {line}"));
        }
        all_runs.push(report);
    }

    let agg = runner::summarise(&all_runs);
    if json_out {
        match serde_json::to_string_pretty(&agg) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("knowledge-gym: cannot serialise summary: {e}"),
        }
    } else {
        println!("\n=== summary ===");
        for line in agg.human_lines() {
            println!("  {line}");
        }
    }

    if agg.pass_rate >= 0.9 {
        0
    } else {
        // Soft non-zero so CI can gate on the threshold while
        // letting interactive iteration still see the table.
        3
    }
}

/// One turn's input + mock evidence in a fixture. Single-turn
/// fixtures have a `Vec<TurnSpec>` of length 1; multi-turn
/// fixtures (Gym Phase 2 of the tool-framework expansion plan)
/// have one entry per user turn. The runner walks them in order,
/// preserving the conversation history between turns.
#[derive(Debug)]
pub(crate) struct TurnSpec {
    pub input: Value,
    pub mock_evidence: Value,
}

#[derive(Debug)]
pub(crate) struct Fixture {
    pub slug: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub turns: Vec<TurnSpec>,
    pub predicates: toml::Value,
}

const EMPTY_MOCK_EVIDENCE: &str =
    r#"{"query": "", "evidence": [], "by_kind_counts": {"corpus": 0, "memory": 0, "note": 0}}"#;

fn load_fixtures(dir: &Path, only: &[String]) -> std::io::Result<Vec<Fixture>> {
    let entries = std::fs::read_dir(dir)?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let slug = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        if !only.is_empty() && !only.iter().any(|s| s == &slug) {
            continue;
        }
        let pass_path = path.join("pass.toml");
        if !pass_path.exists() {
            continue;
        }
        let pass_raw = std::fs::read_to_string(&pass_path)?;
        let predicates: toml::Value = toml::from_str(&pass_raw)
            .map_err(|e| std::io::Error::other(format!("{slug}/pass.toml: {e}")))?;

        // Detect single-turn (input.json) vs multi-turn
        // (input_turn_0.json, input_turn_1.json, ...). The
        // detection rule is presence of `input.json`: when it
        // exists, this is a single-turn fixture and any
        // input_turn_N.json files are ignored (so a fixture
        // author can leave a draft sequence on disk without
        // breaking the single-turn path).
        let turns = if path.join("input.json").exists() {
            vec![load_turn(&path, &slug, "input.json", "mock_evidence.json")?]
        } else {
            load_multi_turn(&path, &slug)?
        };
        if turns.is_empty() {
            continue;
        }
        out.push(Fixture {
            slug,
            path,
            turns,
            predicates,
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Load a single turn's spec from explicit file names. The mock
/// path is optional — when absent, the runner injects an empty
/// evidence envelope (useful for fixtures whose model is expected
/// NOT to call the tool).
fn load_turn(
    dir: &Path,
    slug: &str,
    input_name: &str,
    mock_name: &str,
) -> std::io::Result<TurnSpec> {
    let input_path = dir.join(input_name);
    let mock_path = dir.join(mock_name);
    let input_raw = std::fs::read_to_string(&input_path)?;
    let mock_raw = if mock_path.exists() {
        std::fs::read_to_string(&mock_path)?
    } else {
        EMPTY_MOCK_EVIDENCE.to_string()
    };
    let input: Value = serde_json::from_str(&input_raw)
        .map_err(|e| std::io::Error::other(format!("{slug}/{input_name}: {e}")))?;
    let mock_evidence: Value = serde_json::from_str(&mock_raw)
        .map_err(|e| std::io::Error::other(format!("{slug}/{mock_name}: {e}")))?;
    Ok(TurnSpec {
        input,
        mock_evidence,
    })
}

/// Walk `input_turn_N.json` + `mock_evidence_turn_N.json` pairs
/// for `N = 0, 1, 2, …` until the input file is missing. Returns
/// the contiguous prefix. Gaps (e.g. turn_0 + turn_2 with no
/// turn_1) are not supported — the runner stops at the first
/// missing input.
fn load_multi_turn(dir: &Path, slug: &str) -> std::io::Result<Vec<TurnSpec>> {
    let mut turns = Vec::new();
    for n in 0..64 {
        let input_name = format!("input_turn_{n}.json");
        let mock_name = format!("mock_evidence_turn_{n}.json");
        if !dir.join(&input_name).exists() {
            break;
        }
        turns.push(load_turn(dir, slug, &input_name, &mock_name)?);
    }
    Ok(turns)
}
