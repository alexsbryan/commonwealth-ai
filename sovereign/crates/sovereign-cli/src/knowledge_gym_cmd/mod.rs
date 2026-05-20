//! `sovereign knowledge-gym …` — correctness harness for the
//! unified `knowledge_lookup` tool (Tool-Mastery framework Phase 5).
//!
//! Each fixture under `sovereign-recipes/knowledge-gym/fixtures/`
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
const DEFAULT_FIXTURES_DIR: &str = "sovereign-recipes/knowledge-gym/fixtures";
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
        "sovereign knowledge-gym — correctness harness for knowledge_lookup\n\n\
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
                base_url = iter
                    .next()
                    .map(String::clone)
                    .unwrap_or_else(|| {
                        eprintln!("--base-url requires a value");
                        std::process::exit(2)
                    });
            }
            "--fixtures-dir" => {
                fixtures_dir = iter
                    .next()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
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
        eprintln!("knowledge-gym: no fixtures found under {}", fixtures_dir.display());
        return 1;
    }

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("build http client");
    let cfg = runner::RunnerCfg {
        base_url,
        replays,
    };

    let mut all_runs: Vec<runner::FixtureReport> = Vec::new();
    for fx in &fixtures {
        eprintln!("\n=== fixture: {} ===", fx.slug);
        let report = runner::run_fixture_replays(&client, &cfg, fx).await;
        for line in report.human_lines() {
            eprintln!("  {line}");
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
        eprintln!("\n=== summary ===");
        for line in agg.human_lines() {
            eprintln!("  {line}");
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

#[derive(Debug)]
struct Fixture {
    slug: String,
    path: PathBuf,
    input: Value,
    mock_evidence: Value,
    predicates: toml::Value,
}

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
        let input_path = path.join("input.json");
        let mock_path = path.join("mock_evidence.json");
        let pass_path = path.join("pass.toml");
        if !input_path.exists() || !pass_path.exists() {
            continue;
        }
        let input_raw = std::fs::read_to_string(&input_path)?;
        let mock_raw = if mock_path.exists() {
            std::fs::read_to_string(&mock_path)?
        } else {
            r#"{"query": "", "evidence": [], "by_kind_counts": {"corpus": 0, "memory": 0, "note": 0}}"#
                .to_string()
        };
        let pass_raw = std::fs::read_to_string(&pass_path)?;
        let input: Value = serde_json::from_str(&input_raw).map_err(|e| {
            std::io::Error::other(format!("{slug}/input.json: {e}"))
        })?;
        let mock: Value = serde_json::from_str(&mock_raw).map_err(|e| {
            std::io::Error::other(format!("{slug}/mock_evidence.json: {e}"))
        })?;
        let pass: toml::Value = toml::from_str(&pass_raw).map_err(|e| {
            std::io::Error::other(format!("{slug}/pass.toml: {e}"))
        })?;
        out.push(Fixture {
            slug,
            path,
            input,
            mock_evidence: mock,
            predicates: pass,
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}
