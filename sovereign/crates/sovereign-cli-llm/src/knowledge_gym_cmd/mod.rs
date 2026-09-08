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
//! Each fixture DECLARES the production path it drives, in
//! `pass.toml`'s `production_path` key — see [`ledger::ProductionPath`].
//! The gym replays it through that path, fielding the
//! `knowledge_lookup` invocations with the fixture's canned
//! evidence (no live corpus), and scores the transcript against
//! structural + semantic predicates read off ONE tool ledger.
//!
//! # The retarget (2026-09-07)
//!
//! Until this change every fixture drove `POST /v1/chat/completions`
//! with an OpenAI `tools` array and the predicates read
//! `choices[0].message.tool_calls`. Nothing a user does reaches
//! that surface: `knowledge_query.rs` passes `tools: None` on both
//! synthesis routes, and the two paths that DO offer
//! `knowledge_lookup` render it as a prose line and parse an inline
//! `<tool_call>` marker back out. So a green gym said the daemon's
//! native function-calling adapter held a contract, and said
//! nothing about the product. That surface is still reachable, as
//! `--raw`, and is labelled `not-the-product` wherever it is
//! reported.

mod ledger;
mod production;
mod runner;

pub use ledger::ProductionPath;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const DEFAULT_BASE_URL: &str = "http://localhost:9741";
const DEFAULT_FIXTURES_DIR: &str = "sovereign/bench/knowledge-gym/fixtures";
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_REPLAYS: u32 = 3;
/// The chat model the executor path asks. Every fixture on disk names
/// `"primary"` in its `input.json`, and the raw driver posts that string
/// through to the daemon — so the production driver asks the same one and a
/// before/after comparison is not confounded by the model.
const DEFAULT_CHAT_MODEL: &str = "primary";

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
         USAGE\n  sovereign knowledge-gym run [--fixture SLUG] [--replays N]\n\
         \x20                              [--base-url URL] [--fixtures-dir PATH]\n\
         \x20                              [--raw] [--sabotage KIND] [--json]\n\n\
         Each fixture dir holds input.json + mock_evidence.json + pass.toml,\n\
         and pass.toml DECLARES `production_path` — executor | attached-doc |\n\
         raw. The gym replays the fixture THROUGH that path, fields\n\
         knowledge_lookup with the canned evidence, and scores the path's own\n\
         tool ledger. A fixture declaring no path is refused, not defaulted.\n\n\
         FLAGS\n\
         \x20 --raw            force EVERY fixture onto POST /v1/chat/completions\n\
         \x20                  with an OpenAI tools[] array. No product turn takes\n\
         \x20                  that route, so every report line says\n\
         \x20                  `not-the-product`. Measures the MODEL, never the\n\
         \x20                  product. Cannot be combined with --sabotage.\n\
         \x20 --sabotage KIND  deliberately break the production path, to watch\n\
         \x20                  the lane go red (ARCH §18.1).\n\
         \x20                  `no-tool-offered`: the ReasonWithTools step is\n\
         \x20                  built with an EMPTY tool list, so the production\n\
         \x20                  prompt never offers knowledge_lookup.\n\n\
         See sovereign/bench/knowledge-gym/RUNBOOK.md."
    );
}

async fn run_cmd(args: &[String]) -> i32 {
    let mut base_url = DEFAULT_BASE_URL.to_string();
    let mut fixtures_dir: PathBuf = DEFAULT_FIXTURES_DIR.into();
    let mut only_fixture: Vec<String> = Vec::new();
    let mut replays: u32 = DEFAULT_REPLAYS;
    let mut json_out = false;
    let mut force_raw = false;
    let mut sabotage: Option<production::Sabotage> = None;

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
            "--raw" => {
                force_raw = true;
            }
            "--sabotage" => {
                let v = iter.next().cloned().unwrap_or_else(|| {
                    eprintln!("--sabotage requires a value");
                    std::process::exit(2)
                });
                match production::Sabotage::parse(&v) {
                    Some(sb) => sabotage = Some(sb),
                    None => {
                        eprintln!(
                            "unknown --sabotage kind `{v}` (expected one of: {})",
                            production::Sabotage::ALL.join(", ")
                        );
                        return 2;
                    }
                }
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
    if force_raw && sabotage.is_some() {
        // Both would "work" — --raw wins and the sabotage does nothing — and
        // that is exactly the silent substitution to refuse: a run whose
        // sabotage was ignored looks like a sabotage that failed to go red.
        eprintln!(
            "knowledge-gym: --raw and --sabotage are mutually exclusive. --raw \
             forces the non-product endpoint, which the sabotage does not touch, \
             so the run would report a green sabotage that never applied."
        );
        return 2;
    }
    if let Some(sb) = sabotage {
        let reachable = fixtures
            .iter()
            .filter(|f| f.path == ProductionPath::Executor)
            .count();
        if reachable == 0 {
            eprintln!(
                "knowledge-gym: --sabotage {sb:?} selected, but none of the {} \
                 fixture(s) declare production_path = \"executor\", so the break \
                 would never apply. Refusing rather than reporting a green run.",
                fixtures.len()
            );
            return 2;
        }
    }
    if force_raw {
        eprintln!(
            "knowledge-gym: --raw — every fixture forced onto \
             POST /v1/chat/completions. NOT-THE-PRODUCT: no product turn takes \
             that route, so these numbers are about the model's \
             function-calling adapter."
        );
    }
    if let Some(sb) = sabotage {
        eprintln!("knowledge-gym: --sabotage {sb:?} — the path is deliberately broken.");
    }
    let executor_host = production::ExecutorHost::connect(&base_url, DEFAULT_CHAT_MODEL, sabotage);
    let cfg = runner::RunnerCfg {
        base_url,
        replays,
        force_raw,
        executor_host,
    };

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
    // The summary — INCLUDING the four-verdict distribution — is emitted on
    // both paths. It used to print only in human mode, so the one mode the
    // quality lane actually runs (`--json`) never showed how many replays
    // abstained. ARCH §18.2 as amended: a lane where most replays render
    // could-not-judge has been silenced rather than measured, and that
    // number is the only thing that tells the two apart.
    emit("\n=== summary ===");
    for line in agg.human_lines() {
        emit(&format!("  {line}"));
    }
    if json_out {
        match serde_json::to_string_pretty(&agg) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("knowledge-gym: cannot serialise summary: {e}"),
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
    pub dir: PathBuf,
    /// The production path this fixture DECLARES it drives, read from
    /// `pass.toml`'s `production_path`. Required — see [`load_fixtures`].
    pub path: ProductionPath,
    pub turns: Vec<TurnSpec>,
    pub predicates: toml::Value,
}

const EMPTY_MOCK_EVIDENCE: &str =
    r#"{"query": "", "evidence": [], "by_kind_counts": {"corpus": 0, "memory": 0, "note": 0}}"#;

fn load_fixtures(dir: &Path, only: &[String]) -> std::io::Result<Vec<Fixture>> {
    let entries = std::fs::read_dir(dir)?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path_dir = entry.path();
        if !path_dir.is_dir() {
            continue;
        }
        let slug = path_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        if !only.is_empty() && !only.iter().any(|s| s == &slug) {
            continue;
        }
        let pass_path = path_dir.join("pass.toml");
        if !pass_path.exists() {
            continue;
        }
        let pass_raw = std::fs::read_to_string(&pass_path)?;
        let predicates: toml::Value = toml::from_str(&pass_raw)
            .map_err(|e| std::io::Error::other(format!("{slug}/pass.toml: {e}")))?;

        // REQUIRED, never defaulted. A missing declaration used to be the
        // silent state of the whole gym: every fixture drove the raw endpoint
        // because nothing said otherwise, and a green run read as a statement
        // about the product. Refusing here is what makes "which surface did
        // this measure" impossible to forget (ARCH §7, §18.3).
        let declared = predicates
            .get("production_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "{slug}/pass.toml declares no `production_path` — add one of: {}",
                    ProductionPath::ALL.join(", ")
                ))
            })?;
        let path = ProductionPath::parse(declared).ok_or_else(|| {
            std::io::Error::other(format!(
                "{slug}/pass.toml: production_path = \"{declared}\" is not a known path \
                 (expected one of: {})",
                ProductionPath::ALL.join(", ")
            ))
        })?;
        // A path with no driver is refused HERE, loudly, and the run exits
        // non-zero. It is deliberately NOT a per-replay could-not-judge:
        // ARCH §18.2 as amended — an abstention nobody has watched be
        // necessary is not rigor, and three fixtures reporting
        // could-not-judge for a reason the loader already knew would read as
        // a careful lane while measuring nothing. Give `attached-doc` a
        // driver and delete this arm; until then a fixture cannot silently
        // become nine unjudged replays.
        if path == ProductionPath::AttachedDoc {
            return Err(std::io::Error::other(format!(
                "{slug}/pass.toml declares production_path = \"attached-doc\", which has \
                 no headless driver. `handle_attached_doc_turn` needs a Ready \
                 DocumentAsset plus a DocumentSession on the conversation \
                 (bench_cmd::book_report::dispatch_question is the working recipe), \
                 and the ingest that produces the asset does not fit this lane's \
                 budget. Declare `executor` or `raw`."
            )));
        }

        // Detect single-turn (input.json) vs multi-turn
        // (input_turn_0.json, input_turn_1.json, ...). The
        // detection rule is presence of `input.json`: when it
        // exists, this is a single-turn fixture and any
        // input_turn_N.json files are ignored (so a fixture
        // author can leave a draft sequence on disk without
        // breaking the single-turn path).
        let turns = if path_dir.join("input.json").exists() {
            vec![load_turn(
                &path_dir,
                &slug,
                "input.json",
                "mock_evidence.json",
            )?]
        } else {
            load_multi_turn(&path_dir, &slug)?
        };
        if turns.is_empty() {
            continue;
        }
        // Same rule as the arm above: the `ReasonWithTools` step is
        // single-shot, and the loader can see the turn count. Refuse, rather
        // than run and report N could-not-judge replays for a fact known
        // before the daemon was touched (ARCH §18.2 as amended).
        if path == ProductionPath::Executor && turns.len() != 1 {
            return Err(std::io::Error::other(format!(
                "{slug} declares production_path = \"executor\" with {} turns; the \
                 ReasonWithTools step is single-shot and multi-turn replay through \
                 it is not built. Declare `raw`, or split the fixture.",
                turns.len()
            )));
        }
        out.push(Fixture {
            slug,
            dir: path_dir,
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
    // The `evidence` array is REQUIRED, even when empty. Both drivers read it
    // with `unwrap_or_default()`, so an envelope that misspells the key
    // (`evidences`) would silently become a zero-row lookup and the fixture
    // would score as a clean no-results case — an authoring typo rendered as
    // a passing honesty test (ARCH §18.3). `EMPTY_MOCK_EVIDENCE` writes
    // `"evidence": []` explicitly for exactly this reason; requiring it here
    // is what makes the two drivers' defaults safe.
    if mock_evidence
        .get("evidence")
        .and_then(|v| v.as_array())
        .is_none()
    {
        return Err(std::io::Error::other(format!(
            "{slug}/{mock_name} has no `evidence` array. Write `\"evidence\": []` \
             for a deliberately empty envelope — an absent key is indistinguishable \
             from a misspelled one, and both would score as a clean no-results run."
        )));
    }
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
